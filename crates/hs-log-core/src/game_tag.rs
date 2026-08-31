//! GameTag 命名常量与字符串映射。
//!
//! 与 `hearthstone.enums.GameTag` / `Zone` / `CardType` / `PlayState` / `Step`
//! 对齐的最小子集。Parser 与 Normalizer 共用，避免两套 ID 漂移。
//! 未收录的名字或枚举值返回 `0`（与 Track C 临时映射一致）。

use hs_types::Zone;

pub const PLAYSTATE: i32 = 17;
pub const STEP: i32 = 19;
pub const TURN: i32 = 20;
pub const CURRENT_PLAYER: i32 = 23;
pub const FIRST_PLAYER: i32 = 24;
pub const RESOURCES_USED: i32 = 25;
pub const RESOURCES: i32 = 26;
pub const HERO_ENTITY: i32 = 27;
pub const PLAYER_ID: i32 = 30;
pub const DAMAGE: i32 = 44;
pub const HEALTH: i32 = 45;
pub const ATK: i32 = 47;
pub const COST: i32 = 48;
pub const ZONE: i32 = 49;
pub const CONTROLLER: i32 = 50;
pub const ENTITY_ID: i32 = 53;
pub const CARDTYPE: i32 = 202;
pub const SILENCED: i32 = 188;
pub const ZONE_POSITION: i32 = 263;

pub mod zone {
    pub const INVALID: i32 = 0;
    pub const PLAY: i32 = 1;
    pub const DECK: i32 = 2;
    pub const HAND: i32 = 3;
    pub const GRAVEYARD: i32 = 4;
    pub const REMOVEDFROMGAME: i32 = 5;
    pub const SETASIDE: i32 = 6;
    pub const SECRET: i32 = 7;
}

pub mod card_type {
    pub const INVALID: i32 = 0;
    pub const GAME: i32 = 1;
    pub const PLAYER: i32 = 2;
    pub const HERO: i32 = 3;
    pub const MINION: i32 = 4;
    pub const SPELL: i32 = 5;
    pub const ENCHANTMENT: i32 = 6;
    pub const WEAPON: i32 = 7;
    pub const HERO_POWER: i32 = 10;
    pub const LOCATION: i32 = 39;
}

pub mod play_state {
    pub const INVALID: i32 = 0;
    pub const PLAYING: i32 = 1;
    pub const WINNING: i32 = 2;
    pub const LOSING: i32 = 3;
    pub const WON: i32 = 4;
    pub const LOST: i32 = 5;
    pub const TIED: i32 = 6;
    pub const DISCONNECTED: i32 = 7;
    pub const CONCEDED: i32 = 8;
}

// 与 `hearthstone.enums.Step` 逐值核对（2026-08-21，differential 抓出
// M0 版从 MAIN_READY 起整体错位的 bug，勿凭记忆改动）。
pub mod step {
    pub const INVALID: i32 = 0;
    pub const BEGIN_FIRST: i32 = 1;
    pub const BEGIN_SHUFFLE: i32 = 2;
    pub const BEGIN_DRAW: i32 = 3;
    pub const BEGIN_MULLIGAN: i32 = 4;
    pub const MAIN_BEGIN: i32 = 5;
    pub const MAIN_READY: i32 = 6;
    pub const MAIN_RESOURCE: i32 = 7;
    pub const MAIN_DRAW: i32 = 8;
    pub const MAIN_START: i32 = 9;
    pub const MAIN_ACTION: i32 = 10;
    pub const MAIN_COMBAT: i32 = 11;
    pub const MAIN_END: i32 = 12;
    pub const MAIN_NEXT: i32 = 13;
    pub const FINAL_WRAPUP: i32 = 14;
    pub const FINAL_GAMEOVER: i32 = 15;
    pub const MAIN_CLEANUP: i32 = 16;
    pub const MAIN_START_TRIGGERS: i32 = 17;
}

/// 按 Power.log 标签名查 GameTag 数值；未知名返回 `0`。
pub fn id_by_name(name: &str) -> i32 {
    match name {
        "PLAYSTATE" => PLAYSTATE,
        "STEP" => STEP,
        "TURN" => TURN,
        "CURRENT_PLAYER" => CURRENT_PLAYER,
        "FIRST_PLAYER" => FIRST_PLAYER,
        "RESOURCES_USED" => RESOURCES_USED,
        "RESOURCES" => RESOURCES,
        "HERO_ENTITY" => HERO_ENTITY,
        "PLAYER_ID" => PLAYER_ID,
        "DAMAGE" => DAMAGE,
        "HEALTH" => HEALTH,
        "ATK" => ATK,
        "COST" => COST,
        "ZONE" => ZONE,
        "CONTROLLER" => CONTROLLER,
        "ENTITY_ID" => ENTITY_ID,
        "CARDTYPE" => CARDTYPE,
        "ZONE_POSITION" => ZONE_POSITION,
        // 其余具名 tag 查全量生成表；纯数字 tag（如 `tag=467`）保留数值。
        // 两者都必须保住身份，否则塌缩到 0 后在实体 tags 里互相覆盖。
        other => {
            let other = other.trim();
            crate::game_tag_table::lookup(other)
                .or_else(|| other.parse().ok())
                .unwrap_or(0)
        }
    }
}

/// 将 `tag=NAME value=VALUE` 的 VALUE 归一为 i32。
/// 能解析为整数则直接用；否则按标签枚举名映射；仍未知则 `0`。
pub fn value_by_name(tag: &str, value: &str) -> i32 {
    if let Ok(n) = value.parse() {
        return n;
    }
    match (tag, value) {
        ("ZONE", "INVALID") => zone::INVALID,
        ("ZONE", "PLAY") => zone::PLAY,
        ("ZONE", "DECK") => zone::DECK,
        ("ZONE", "HAND") => zone::HAND,
        ("ZONE", "GRAVEYARD") => zone::GRAVEYARD,
        ("ZONE", "REMOVEDFROMGAME") => zone::REMOVEDFROMGAME,
        ("ZONE", "SETASIDE") => zone::SETASIDE,
        ("ZONE", "SECRET") => zone::SECRET,
        ("CARDTYPE", "INVALID") => card_type::INVALID,
        ("CARDTYPE", "GAME") => card_type::GAME,
        ("CARDTYPE", "PLAYER") => card_type::PLAYER,
        ("CARDTYPE", "HERO") => card_type::HERO,
        ("CARDTYPE", "MINION") => card_type::MINION,
        ("CARDTYPE", "SPELL") => card_type::SPELL,
        ("CARDTYPE", "ENCHANTMENT") => card_type::ENCHANTMENT,
        ("CARDTYPE", "WEAPON") => card_type::WEAPON,
        ("CARDTYPE", "HERO_POWER") => card_type::HERO_POWER,
        ("CARDTYPE", "LOCATION") => card_type::LOCATION,
        ("PLAYSTATE", "INVALID") => play_state::INVALID,
        ("PLAYSTATE", "PLAYING") => play_state::PLAYING,
        ("PLAYSTATE", "WINNING") => play_state::WINNING,
        ("PLAYSTATE", "LOSING") => play_state::LOSING,
        ("PLAYSTATE", "WON") => play_state::WON,
        ("PLAYSTATE", "LOST") => play_state::LOST,
        ("PLAYSTATE", "TIED") => play_state::TIED,
        ("PLAYSTATE", "DISCONNECTED") => play_state::DISCONNECTED,
        ("PLAYSTATE", "CONCEDED") => play_state::CONCEDED,
        ("STEP", "INVALID") => step::INVALID,
        ("STEP", "BEGIN_FIRST") => step::BEGIN_FIRST,
        ("STEP", "BEGIN_SHUFFLE") => step::BEGIN_SHUFFLE,
        ("STEP", "BEGIN_DRAW") => step::BEGIN_DRAW,
        ("STEP", "BEGIN_MULLIGAN") => step::BEGIN_MULLIGAN,
        ("STEP", "MAIN_BEGIN") => step::MAIN_BEGIN,
        ("STEP", "MAIN_READY") => step::MAIN_READY,
        ("STEP", "MAIN_RESOURCE") => step::MAIN_RESOURCE,
        ("STEP", "MAIN_DRAW") => step::MAIN_DRAW,
        ("STEP", "MAIN_START") => step::MAIN_START,
        ("STEP", "MAIN_ACTION") => step::MAIN_ACTION,
        ("STEP", "MAIN_COMBAT") => step::MAIN_COMBAT,
        ("STEP", "MAIN_END") => step::MAIN_END,
        ("STEP", "MAIN_NEXT") => step::MAIN_NEXT,
        ("STEP", "FINAL_WRAPUP") => step::FINAL_WRAPUP,
        ("STEP", "FINAL_GAMEOVER") => step::FINAL_GAMEOVER,
        ("STEP", "MAIN_CLEANUP") => step::MAIN_CLEANUP,
        ("STEP", "MAIN_START_TRIGGERS") => step::MAIN_START_TRIGGERS,
        // 其余符号值查全量生成表（tag 的枚举类型 → 值名 → 数值），
        // 覆盖 NEXT_STEP/MULLIGAN_STATE/STATE 等，避免塌缩为 0。
        _ => game_tag_value_lookup(id_by_name(tag), value),
    }
}

/// 按 tag 数值 id 查符号值（生成表 `game_tag_value_table`）。
fn game_tag_value_lookup(tag_id: i32, value: &str) -> i32 {
    crate::game_tag_value_table::lookup(tag_id, value.trim()).unwrap_or(0)
}

pub fn zone_from_value(value: i32) -> Zone {
    match value {
        zone::PLAY => Zone::Play,
        zone::DECK => Zone::Deck,
        zone::HAND => Zone::Hand,
        zone::GRAVEYARD => Zone::Graveyard,
        zone::REMOVEDFROMGAME => Zone::RemovedFromGame,
        zone::SETASIDE => Zone::SetAside,
        zone::SECRET => Zone::Secret,
        zone::INVALID => Zone::Invalid,
        other => Zone::Unknown(other),
    }
}

pub fn zone_to_value(zone_value: Zone) -> i32 {
    match zone_value {
        Zone::Play => zone::PLAY,
        Zone::Deck => zone::DECK,
        Zone::Hand => zone::HAND,
        Zone::Graveyard => zone::GRAVEYARD,
        Zone::RemovedFromGame => zone::REMOVEDFROMGAME,
        Zone::SetAside => zone::SETASIDE,
        Zone::Secret => zone::SECRET,
        Zone::Invalid => zone::INVALID,
        Zone::Unknown(raw) => raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_c_tag_ids_unchanged() {
        assert_eq!(id_by_name("PLAYSTATE"), 17);
        assert_eq!(id_by_name("STEP"), 19);
        assert_eq!(id_by_name("TURN"), 20);
        assert_eq!(id_by_name("DAMAGE"), 44);
        assert_eq!(id_by_name("HEALTH"), 45);
        assert_eq!(id_by_name("ZONE"), 49);
        assert_eq!(id_by_name("CONTROLLER"), 50);
        assert_eq!(id_by_name("ENTITY_ID"), 53);
        assert_eq!(id_by_name("CARDTYPE"), 202);
        assert_eq!(id_by_name("COST"), 48);
        assert_eq!(id_by_name("UNKNOWN_TAG"), 0);
    }

    #[test]
    fn track_c_named_values_unchanged() {
        assert_eq!(value_by_name("ZONE", "PLAY"), 1);
        assert_eq!(value_by_name("ZONE", "DECK"), 2);
        assert_eq!(value_by_name("ZONE", "HAND"), 3);
        assert_eq!(value_by_name("ZONE", "GRAVEYARD"), 4);
        assert_eq!(value_by_name("CARDTYPE", "GAME"), 1);
        assert_eq!(value_by_name("CARDTYPE", "PLAYER"), 2);
        assert_eq!(value_by_name("CARDTYPE", "HERO"), 3);
        assert_eq!(card_type::LOCATION, 39);
        assert_eq!(value_by_name("CARDTYPE", "LOCATION"), 39);
        assert_eq!(game_tag_value_lookup(CARDTYPE, "LOCATION"), 39);
        assert_eq!(value_by_name("PLAYSTATE", "PLAYING"), 1);
        assert_eq!(value_by_name("STEP", "BEGIN_MULLIGAN"), 4);
        assert_eq!(value_by_name("DAMAGE", "0"), 0);
        assert_eq!(value_by_name("HEALTH", "30"), 30);
        assert_eq!(value_by_name("STEP", "NOT_A_STEP"), 0);
    }
}
