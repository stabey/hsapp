//! `hs-pools::CardQuery` 的生产适配器(M3 Track D)。
//!
//! 依赖方向:领域 crate 互不反向依赖,desktop 负责接线(与 M2 的
//! Decks.log→hs-decks 接线同一惯例)——把 `hs-pools` 的过滤条件翻译成
//! `hs-card-db::PoolCardFilter`,行结果翻译回 `CardMeta`。

use hs_card_db::{CardRepository, PoolCardFilter, SqliteCardRepository};
use hs_pools::{CardFilter, CardMeta, CardQuery, PoolError};
use hs_sim::{CardLookup, SimCardDefinition};
use hs_types::{BuildId, CardId, CardType, DbfId};
use std::sync::Mutex;

/// SQLite 卡牌库适配器(Connection 非 Sync,用 Mutex 串行化查询)。
pub struct SqliteCardQuery {
    repo: Mutex<SqliteCardRepository>,
}

impl SqliteCardQuery {
    pub fn new(repo: SqliteCardRepository) -> Self {
        Self {
            repo: Mutex::new(repo),
        }
    }

    /// Whether the database contains the exact observed Build.
    ///
    /// Pool queries against an absent Build otherwise look exactly like a
    /// legitimate empty result, so Advisor resource assembly must preflight
    /// this before starting a search.
    #[allow(dead_code)] // `pools_selfcheck` path-includes this adapter without Advisor.
    pub fn has_build(&self, build: BuildId) -> Result<bool, String> {
        let repo = self.lock_repo()?;
        repo.list_builds()
            .map(|builds| builds.binary_search(&build).is_ok())
            .map_err(|error| error.to_string())
    }

    /// Resolve one deckstring DBF id using the caller's exact Build.
    #[allow(dead_code)] // `pools_selfcheck` path-includes this adapter without Advisor.
    pub fn card_id_by_dbf(&self, build: BuildId, dbf_id: DbfId) -> Result<Option<CardId>, String> {
        let repo = self.lock_repo()?;
        repo.card_by_dbf(build, dbf_id.0, "enUS")
            .map(|row| row.map(|(card_id, _)| card_id))
            .map_err(|error| error.to_string())
    }

    /// Resolve a hero/deck class using the caller's exact Build.
    #[allow(dead_code)] // `pools_selfcheck` path-includes this adapter without Advisor.
    pub fn card_class_by_dbf(
        &self,
        build: BuildId,
        dbf_id: DbfId,
    ) -> Result<Option<String>, String> {
        let repo = self.lock_repo()?;
        let Some((card_id, _)) = repo
            .card_by_dbf(build, dbf_id.0, "enUS")
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        repo.card_class(build, &card_id)
            .map_err(|error| error.to_string())
    }

    fn lock_repo(&self) -> Result<std::sync::MutexGuard<'_, SqliteCardRepository>, String> {
        self.repo
            .lock()
            .map_err(|error| format!("卡牌库锁中毒: {error}"))
    }

    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition> {
        let repo = self.lock_repo().ok()?;
        let definition = repo.card(build, card_id).ok()?;
        if definition.build_id != build || &definition.card_id != card_id {
            return None;
        }
        project_sim_card(definition)
    }
}

impl CardQuery for SqliteCardQuery {
    fn query(&self, build: BuildId, filter: &CardFilter) -> Result<Vec<CardMeta>, PoolError> {
        let db_filter = PoolCardFilter {
            collectible: filter.collectible,
            card_type: filter.card_type.clone(),
            card_classes: filter.card_classes.clone(),
            rarity: filter.rarity.clone(),
            card_sets: filter.card_sets.clone(),
            cost_min: filter.cost_min,
            cost_max: filter.cost_max,
        };
        let repo = self.lock_repo().map_err(PoolError::CardQuery)?;
        let rows = repo
            .cards_for_pool(build, &db_filter)
            .map_err(|e| PoolError::CardQuery(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| CardMeta {
                card_id: row.card_id,
                dbf_id: row.dbf_id.map(DbfId),
                card_type: row.card_type,
                card_set: row.card_set,
                rarity: row.rarity,
                card_class: row.card_class,
                cost: row.cost,
                collectible: row.collectible,
            })
            .collect())
    }
}

impl CardLookup for SqliteCardQuery {
    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition> {
        SqliteCardQuery::sim_card(self, build, card_id)
    }
}

fn project_sim_card(definition: hs_card_db::CardDefinition) -> Option<SimCardDefinition> {
    let card_type = card_type_from_db(definition.card_type.as_deref()?)?;
    let (cost, attack, health, durability) = match card_type {
        CardType::Minion => (definition.cost?, definition.attack?, definition.health?, 0),
        CardType::Spell => (definition.cost?, 0, 0, 0),
        CardType::Weapon => (definition.cost?, definition.attack?, 0, definition.health?),
        CardType::HeroPower => (definition.cost?, 0, 0, 0),
        CardType::Location => (definition.cost?, 0, 0, definition.health?),
        CardType::Hero => (0, definition.attack.unwrap_or(0), definition.health?, 0),
        // These are recognized database values, but not complete playable
        // definitions for strict current-turn simulation.
        CardType::Game
        | CardType::Player
        | CardType::Enchantment
        | CardType::Invalid
        | CardType::Unknown(_) => return None,
    };
    Some(SimCardDefinition {
        card_id: definition.card_id,
        card_type,
        cost,
        attack,
        health,
        durability,
    })
}

fn card_type_from_db(raw: &str) -> Option<CardType> {
    match raw {
        "GAME" => Some(CardType::Game),
        "PLAYER" => Some(CardType::Player),
        "HERO" => Some(CardType::Hero),
        "MINION" => Some(CardType::Minion),
        "SPELL" => Some(CardType::Spell),
        "ENCHANTMENT" => Some(CardType::Enchantment),
        "WEAPON" => Some(CardType::Weapon),
        "HERO_POWER" => Some(CardType::HeroPower),
        "LOCATION" => Some(CardType::Location),
        "INVALID" => Some(CardType::Invalid),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_card_db::{CardRecord, GameBuildRecord};

    const BUILD: BuildId = BuildId(249_896);

    fn record(
        card_id: &str,
        dbf_id: i64,
        card_type: &str,
        cost: Option<i32>,
        attack: Option<i32>,
        health: Option<i32>,
    ) -> CardRecord {
        CardRecord {
            build_id: BUILD,
            card_id: CardId::new(card_id),
            dbf_id: Some(dbf_id),
            card_type: Some(card_type.into()),
            card_set: Some("TEST".into()),
            rarity: None,
            card_class_json: Some("[\"HUNTER\"]".into()),
            cost,
            attack,
            health,
            collectible: true,
            raw_json: "{}".into(),
            localizations: Vec::new(),
        }
    }

    fn query(records: &[CardRecord]) -> SqliteCardQuery {
        let mut repo = SqliteCardRepository::open_in_memory().expect("memory database");
        repo.import_build(
            &GameBuildRecord {
                build_id: BUILD,
                version_text: Some("test".into()),
                source_manifest: "memory://advisor-resources".into(),
                source_hash: "test".into(),
            },
            records,
        )
        .expect("seed exact Build");
        SqliteCardQuery::new(repo)
    }

    #[test]
    fn sqlite_query_is_send_sync_and_checks_exact_build() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SqliteCardQuery>();

        let query = query(&[record("MINION", 1, "MINION", Some(2), Some(3), Some(4))]);
        assert!(query.has_build(BUILD).unwrap());
        assert!(!query.has_build(BuildId(BUILD.0 + 1)).unwrap());
        assert_eq!(
            query.card_id_by_dbf(BUILD, DbfId(1)).unwrap(),
            Some(CardId::new("MINION"))
        );
        assert_eq!(
            query.card_class_by_dbf(BUILD, DbfId(1)).unwrap(),
            Some("HUNTER".into())
        );
    }

    #[test]
    fn strict_card_projection_maps_weapon_and_location_health_to_durability() {
        let query = query(&[
            record("MINION", 1, "MINION", Some(2), Some(3), Some(4)),
            record("WEAPON", 2, "WEAPON", Some(3), Some(5), Some(2)),
            record("LOCATION", 3, "LOCATION", Some(4), None, Some(3)),
            record("SPELL", 4, "SPELL", Some(1), None, None),
        ]);

        let minion = CardLookup::sim_card(&query, BUILD, &CardId::new("MINION")).unwrap();
        assert_eq!(
            (minion.card_type, minion.cost, minion.attack, minion.health),
            (CardType::Minion, 2, 3, 4)
        );
        assert_eq!(minion.durability, 0);

        let weapon = CardLookup::sim_card(&query, BUILD, &CardId::new("WEAPON")).unwrap();
        assert_eq!(
            (
                weapon.card_type,
                weapon.attack,
                weapon.health,
                weapon.durability
            ),
            (CardType::Weapon, 5, 0, 2)
        );

        let location = CardLookup::sim_card(&query, BUILD, &CardId::new("LOCATION")).unwrap();
        assert_eq!(
            (
                location.card_type,
                location.attack,
                location.health,
                location.durability
            ),
            (CardType::Location, 0, 0, 3)
        );

        let spell = CardLookup::sim_card(&query, BUILD, &CardId::new("SPELL")).unwrap();
        assert_eq!(
            (spell.card_type, spell.cost, spell.attack, spell.health),
            (CardType::Spell, 1, 0, 0)
        );
    }

    #[test]
    fn strict_card_projection_rejects_missing_required_or_unknown_data() {
        let query = query(&[
            record("BAD_MINION", 1, "MINION", Some(2), None, Some(4)),
            record("BAD_LOCATION", 2, "LOCATION", Some(2), None, None),
            record("FUTURE", 3, "FUTURE_TYPE", Some(1), Some(1), Some(1)),
        ]);

        for card in ["BAD_MINION", "BAD_LOCATION", "FUTURE"] {
            assert!(CardLookup::sim_card(&query, BUILD, &CardId::new(card)).is_none());
        }
        assert!(
            CardLookup::sim_card(&query, BuildId(BUILD.0 + 1), &CardId::new("BAD_MINION"))
                .is_none()
        );
    }
}
