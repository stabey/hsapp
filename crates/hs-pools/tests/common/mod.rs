//! 测试用内存卡牌库与手造上下文。

use hs_observed_state::{EntityState, ObservedGameState};
use hs_pools::{card_matches_filter, CardFilter, CardMeta, CardQuery, PoolContext, PoolError};
use hs_types::{BuildId, CardId, DbfId, EntityId, FormatType, GameMode, PlayerId, Zone};

pub const BUILD: BuildId = BuildId(249896);

pub struct MemoryCatalog {
    cards: Vec<CardMeta>,
}

impl MemoryCatalog {
    pub fn hearthstone_subset() -> Self {
        Self {
            cards: vec![
                meta("CS2_029", 315, "SPELL", "CORE", "MAGE", Some(4), true, None),
                meta("CS2_023", 405, "SPELL", "CORE", "MAGE", Some(3), true, None),
                meta(
                    "CS2_032",
                    1004,
                    "SPELL",
                    "CORE",
                    "MAGE",
                    Some(7),
                    true,
                    None,
                ),
                meta(
                    "CS2_122",
                    1401,
                    "MINION",
                    "CORE",
                    "NEUTRAL",
                    Some(3),
                    true,
                    None,
                ),
                meta(
                    "EX1_116",
                    559,
                    "MINION",
                    "LEGACY",
                    "NEUTRAL",
                    Some(5),
                    true,
                    Some("LEGENDARY"),
                ),
                meta(
                    "CS2_168",
                    191,
                    "MINION",
                    "CORE",
                    "NEUTRAL",
                    Some(1),
                    true,
                    None,
                ),
                meta(
                    "CS2_142",
                    1133,
                    "MINION",
                    "LEGACY",
                    "NEUTRAL",
                    Some(2),
                    true,
                    None,
                ),
                meta("EX1_277", 564, "SPELL", "CORE", "MAGE", Some(1), true, None),
                meta(
                    "CS2_034",
                    807,
                    "HERO_POWER",
                    "CORE",
                    "MAGE",
                    Some(2),
                    false,
                    None,
                ),
                meta(
                    "NEW1_019",
                    1073,
                    "MINION",
                    "CORE",
                    "NEUTRAL",
                    Some(2),
                    true,
                    None,
                ),
                meta(
                    "EX1_246",
                    766,
                    "SPELL",
                    "CORE",
                    "SHAMAN",
                    Some(4),
                    true,
                    None,
                ),
                meta(
                    "FP1_004",
                    1783,
                    "MINION",
                    "NAXX",
                    "NEUTRAL",
                    Some(2),
                    true,
                    None,
                ),
                meta(
                    "CS2_056",
                    300,
                    "HERO_POWER",
                    "CORE",
                    "WARLOCK",
                    Some(2),
                    false,
                    None,
                ),
                meta(
                    "EX1_066",
                    906,
                    "MINION",
                    "CORE",
                    "NEUTRAL",
                    Some(2),
                    true,
                    None,
                ),
                meta(
                    "CS2_213",
                    445,
                    "MINION",
                    "LEGACY",
                    "NEUTRAL",
                    Some(6),
                    true,
                    None,
                ),
                meta(
                    "CS2_062",
                    950,
                    "SPELL",
                    "CORE",
                    "WARLOCK",
                    Some(4),
                    true,
                    None,
                ),
                meta(
                    "EX1_279",
                    1087,
                    "SPELL",
                    "LEGACY",
                    "MAGE",
                    Some(10),
                    true,
                    None,
                ),
            ],
        }
    }
}

impl CardQuery for MemoryCatalog {
    fn query(&self, _build: BuildId, filter: &CardFilter) -> Result<Vec<CardMeta>, PoolError> {
        let mut out: Vec<CardMeta> = self
            .cards
            .iter()
            .filter(|card| card_matches_filter(card, filter))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.card_id.cmp(&b.card_id));
        Ok(out)
    }
}

#[allow(clippy::too_many_arguments)]
fn meta(
    card_id: &str,
    dbf: i64,
    card_type: &str,
    card_set: &str,
    card_class: &str,
    cost: Option<i32>,
    collectible: bool,
    rarity: Option<&str>,
) -> CardMeta {
    CardMeta {
        card_id: CardId::new(card_id),
        dbf_id: Some(DbfId(dbf)),
        card_type: Some(card_type.into()),
        card_set: Some(card_set.into()),
        rarity: rarity.map(str::to_string),
        card_class: Some(card_class.into()),
        cost,
        collectible,
    }
}

pub fn mage_context(source: &str) -> PoolContext {
    PoolContext {
        build_id: BUILD,
        game_mode: GameMode::Ranked,
        format: FormatType::Standard,
        player_class: vec!["MAGE".into()],
        original_class: Vec::new(),
        source_card: CardId::new(source),
        source_entity: None,
        current_cost: None,
        state_revision: 0,
    }
}

pub fn put_entity(
    state: &mut ObservedGameState,
    entity_id: i32,
    card_id: Option<&str>,
    zone: Zone,
    controller: Option<i32>,
) {
    let mut entity = EntityState::new(EntityId(entity_id), card_id.map(CardId::new), zone);
    entity.controller = controller.map(PlayerId);
    state.entities.insert(entity.entity_id, entity);
}

pub fn friendly_state() -> ObservedGameState {
    let mut state = ObservedGameState::default();
    state.friendly_player = Some(PlayerId(1));
    state.revision = 3;
    state
}
