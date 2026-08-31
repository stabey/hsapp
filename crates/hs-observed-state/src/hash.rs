//! 观察状态的稳定哈希（重放确定性基线）。

use crate::ObservedGameState;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// 对语义字段做稳定 JSON 序列化后计算 SHA-256，返回 64 字符小写 hex。
///
/// **覆盖**：`build_id`、`game_id`、`phase`、`turn`、`current_player`、
/// `friendly_player`、`winner`、`entities`、`players`、`block_stack`、
/// `options`、`choices`、`sent_choice_echoes`、`public_counters`、`revision`。
///
/// **排除**：`warnings` 文本与 `warning_count`（措辞/封顶策略变化不得破坏基线）。
///
/// 序列化字段序取结构体声明序；容器为 `BTreeMap`/`Vec`，不依赖 `HashMap` 迭代序。
pub fn state_hash(state: &ObservedGameState) -> String {
    let view = StateHashView {
        build_id: &state.build_id,
        game_id: &state.game_id,
        phase: state.phase,
        turn: state.turn,
        current_player: &state.current_player,
        friendly_player: &state.friendly_player,
        winner: &state.winner,
        entities: &state.entities,
        players: &state.players,
        block_stack: &state.block_stack,
        options: &state.options,
        choices: &state.choices,
        sent_choice_echoes: &state.sent_choice_echoes,
        public_counters: &state.public_counters,
        revision: state.revision,
    };
    let bytes = serde_json::to_vec(&view).expect("ObservedGameState 语义字段应可序列化");
    hex_encode(&Sha256::digest(bytes))
}

/// 仅用于 hash 的投影，字段声明序即序列化序。
#[derive(Serialize)]
struct StateHashView<'a> {
    build_id: &'a Option<hs_types::BuildId>,
    game_id: &'a Option<hs_types::GameId>,
    phase: hs_types::GamePhase,
    turn: u32,
    current_player: &'a Option<hs_types::PlayerId>,
    friendly_player: &'a Option<hs_types::PlayerId>,
    winner: &'a Option<hs_types::PlayerId>,
    entities: &'a std::collections::BTreeMap<hs_types::EntityId, crate::EntityState>,
    players: &'a std::collections::BTreeMap<i32, crate::PlayerState>,
    block_stack: &'a [crate::BlockFrame],
    options: &'a Option<crate::OptionsState>,
    choices: &'a [crate::ChoicesState],
    sent_choice_echoes: &'a [crate::SentChoiceEcho],
    public_counters: &'a std::collections::BTreeMap<String, i64>,
    revision: u64,
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
    use super::hex_encode;
    use sha2::{Digest, Sha256};

    #[test]
    fn hex_encode_sha256_empty_is_stable() {
        assert_eq!(
            hex_encode(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
