//! PoolContext:池求值时的上下文输入(§12.4)。
//! 动态池对状态的引用不进本结构——resolve 时以 `Option<&ObservedGameState>`
//! 单独传入,context 只装可哈希的标量键。

use hs_types::{BuildId, CardId, EntityId, FormatType, GameMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 池声明式依赖的上下文键(PoolSpec.context_inputs;BTreeSet 保证序稳定)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKey {
    Build,
    GameMode,
    Format,
    PlayerClass,
    OriginalClass,
    SourceCard,
    SourceEntity,
    CurrentCost,
    /// 以下为动态状态依赖:resolve 时必须提供状态快照,否则 StateRequired。
    Deck,
    Hand,
    Graveyard,
    PlayedHistory,
    GeneratedHistory,
    BoardEntities,
}

/// 池求值上下文(§12.4,冻结)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolContext {
    pub build_id: BuildId,
    pub game_mode: GameMode,
    pub format: FormatType,
    /// 求值方职业码(如 "MAGE";双职业场景多值)。
    pub player_class: Vec<String>,
    /// 卡牌原始职业(职业变换场景;通常与 player_class 相同)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub original_class: Vec<String>,
    pub source_card: CardId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_entity: Option<EntityId>,
    /// 来源卡当前费用(费用相关池,如"随机一张费用等于 X 的牌")。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_cost: Option<i32>,
    /// 状态修订号(动态池快照锚点;无状态池为 0)。
    pub state_revision: u64,
}

/// 对 [`PoolContext`] 做 serde_json 序列化(字段声明序)后算 sha256,返回 64 位小写 hex。
///
/// `skip_serializing_if` 仍生效:空 `original_class` / 缺省 `source_entity` /
/// `current_cost` 不进字节流。同一上下文多次调用结果逐字节一致。
pub fn context_hash(ctx: &PoolContext) -> String {
    let bytes = serde_json::to_vec(ctx).expect("PoolContext 应可 JSON 序列化");
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_types::{BuildId, CardId, FormatType, GameMode};

    fn sample_ctx() -> PoolContext {
        PoolContext {
            build_id: BuildId(249896),
            game_mode: GameMode::Ranked,
            format: FormatType::Standard,
            player_class: vec!["MAGE".into()],
            original_class: Vec::new(),
            source_card: CardId::new("CS2_023"),
            source_entity: None,
            current_cost: None,
            state_revision: 0,
        }
    }

    #[test]
    fn context_hash_is_stable() {
        let a = context_hash(&sample_ctx());
        let b = context_hash(&sample_ctx());
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn context_hash_changes_with_format() {
        let mut ctx = sample_ctx();
        let standard = context_hash(&ctx);
        ctx.format = FormatType::Wild;
        assert_ne!(standard, context_hash(&ctx));
    }
}
