//! SQLite 实现 [`DeckRepository`] + [`GameHistoryRepository`]。

use crate::migrate::apply_deck_migrations;
use crate::{
    Deck, DeckDraft, DeckRepository, DeckStats, DecksError, GameFilter, GameHistoryRepository,
    GameRecord,
};
use hs_deckstrings::{decode, encode, DeckList};
use hs_types::{DeckId, FormatType, GameId, PlayerId};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

/// 单一 SQLite 仓储,同时实现牌组与对局历史契约。
pub struct SqliteDeckStore {
    conn: Mutex<Connection>,
}

impl SqliteDeckStore {
    pub fn open_in_memory() -> Result<Self, DecksError> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        apply_deck_migrations(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, DecksError> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        apply_deck_migrations(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DecksError> {
        self.conn
            .lock()
            .map_err(|_| DecksError::Invalid("sqlite connection lock poisoned".into()))
    }
}

impl DeckRepository for SqliteDeckStore {
    fn create(&self, draft: &DeckDraft) -> Result<Deck, DecksError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let id = insert_deck(&tx, draft, false)?;
        tx.commit()?;
        drop(conn);
        self.get(id)
    }

    fn update(&self, id: DeckId, draft: &DeckDraft) -> Result<Deck, DecksError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let exists: bool = tx
            .prepare("SELECT 1 FROM decks WHERE id = ?1")?
            .exists([id.0])?;
        if !exists {
            return Err(DecksError::NotFound(id));
        }
        let canonical = encode(&draft.list)?;
        tx.execute(
            "UPDATE decks SET
                 name = ?1,
                 class = ?2,
                 format = ?3,
                 canonical_deckstring = ?4,
                 updated_at = datetime('now')
             WHERE id = ?5",
            params![
                draft.name,
                draft.class,
                draft.list.format.to_raw(),
                canonical,
                id.0
            ],
        )?;
        insert_version(&tx, id, &canonical, &draft.list)?;
        tx.commit()?;
        drop(conn);
        self.get(id)
    }

    fn copy(&self, id: DeckId, new_name: &str) -> Result<Deck, DecksError> {
        let src = self.get(id)?;
        let draft = DeckDraft {
            name: new_name.to_string(),
            class: src.class,
            format: src.list.format,
            list: src.list,
        };
        self.create(&draft)
    }

    fn set_archived(&self, id: DeckId, archived: bool) -> Result<(), DecksError> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE decks SET archived = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![i32::from(archived), id.0],
        )?;
        if n == 0 {
            return Err(DecksError::NotFound(id));
        }
        if archived {
            conn.execute(
                "UPDATE decks SET is_active = 0 WHERE id = ?1 AND is_active = 1",
                [id.0],
            )?;
        }
        Ok(())
    }

    fn get(&self, id: DeckId) -> Result<Deck, DecksError> {
        let conn = self.lock()?;
        read_deck(&conn, id)
    }

    fn list(&self, include_archived: bool) -> Result<Vec<Deck>, DecksError> {
        let conn = self.lock()?;
        let sql = if include_archived {
            "SELECT id FROM decks ORDER BY updated_at DESC, id DESC"
        } else {
            "SELECT id FROM decks WHERE archived = 0 ORDER BY updated_at DESC, id DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let ids = stmt
            .query_map([], |row| row.get::<_, i64>(0).map(DeckId))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| read_deck(&conn, id)).collect()
    }

    fn set_active(&self, id: Option<DeckId>) -> Result<(), DecksError> {
        let conn = self.lock()?;
        if let Some(id) = id {
            let exists: bool = conn
                .prepare("SELECT 1 FROM decks WHERE id = ?1")?
                .exists([id.0])?;
            if !exists {
                return Err(DecksError::NotFound(id));
            }
            conn.execute("UPDATE decks SET is_active = 0 WHERE is_active = 1", [])?;
            conn.execute("UPDATE decks SET is_active = 1 WHERE id = ?1", [id.0])?;
        } else {
            conn.execute("UPDATE decks SET is_active = 0 WHERE is_active = 1", [])?;
        }
        Ok(())
    }

    fn active(&self) -> Result<Option<Deck>, DecksError> {
        let conn = self.lock()?;
        let id: Option<i64> = conn
            .query_row("SELECT id FROM decks WHERE is_active = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        match id {
            Some(id) => Ok(Some(read_deck(&conn, DeckId(id))?)),
            None => Ok(None),
        }
    }

    fn upsert_observed_deck(&self, name: &str, deckstring: &str) -> Result<DeckId, DecksError> {
        let list = decode(deckstring)?;
        let canonical = encode(&list)?;
        {
            let conn = self.lock()?;
            let existing: Option<i64> = conn
                .query_row(
                    "SELECT id FROM decks
                     WHERE canonical_deckstring = ?1
                     ORDER BY archived ASC, id ASC
                     LIMIT 1",
                    [&canonical],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                drop(conn);
                let deck_id = DeckId(id);
                self.set_archived(deck_id, false)?;
                return Ok(deck_id);
            }
        }

        let draft = DeckDraft {
            name: name.to_string(),
            class: String::new(),
            format: list.format,
            list,
        };
        let created = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            // 收藏导入不抢活动牌组;排队激活走 ingest / set_active。
            let id = insert_deck(&tx, &draft, false)?;
            tx.commit()?;
            id
        };
        Ok(created)
    }

    fn ingest_observed_deck(&self, name: &str, deckstring: &str) -> Result<DeckId, DecksError> {
        let id = self.upsert_observed_deck(name, deckstring)?;
        self.set_active(Some(id))?;
        Ok(id)
    }
}

impl GameHistoryRepository for SqliteDeckStore {
    fn record_game(&self, rec: &GameRecord) -> Result<(), DecksError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let game_id = game_id_sql(rec.game_id)?;
        tx.execute(
            "INSERT INTO game_sessions (
                 game_id, deck_id, started_at, ended_at, raw_log_path, raw_log_sha256
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(game_id) DO UPDATE SET
                 deck_id = excluded.deck_id,
                 started_at = excluded.started_at,
                 ended_at = excluded.ended_at,
                 raw_log_path = excluded.raw_log_path,
                 raw_log_sha256 = excluded.raw_log_sha256",
            params![
                game_id,
                rec.deck_id.map(|d| d.0),
                rec.started_at,
                rec.ended_at,
                rec.raw_log_path,
                rec.raw_log_sha256,
            ],
        )?;

        tx.execute("DELETE FROM game_players WHERE game_id = ?1", [game_id])?;
        upsert_player(
            &tx,
            game_id,
            "friendly",
            rec.friendly_player,
            rec.friendly_class.as_deref(),
        )?;
        upsert_player(
            &tx,
            game_id,
            "opposing",
            opposing_player(rec.friendly_player),
            rec.opposing_class.as_deref(),
        )?;

        tx.execute(
            "INSERT INTO game_results (game_id, winner) VALUES (?1, ?2)
             ON CONFLICT(game_id) DO UPDATE SET winner = excluded.winner",
            params![game_id, rec.winner.map(|p| p.0)],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn list_games(&self, filter: &GameFilter) -> Result<Vec<GameRecord>, DecksError> {
        let conn = self.lock()?;
        let mut sql = String::from(
            "SELECT s.game_id, s.deck_id, s.started_at, s.ended_at,
                    s.raw_log_path, s.raw_log_sha256,
                    f.class, o.class, f.player_id, r.winner
             FROM game_sessions s
             LEFT JOIN game_players f ON f.game_id = s.game_id AND f.role = 'friendly'
             LEFT JOIN game_players o ON o.game_id = s.game_id AND o.role = 'opposing'
             LEFT JOIN game_results r ON r.game_id = s.game_id",
        );
        if filter.deck_id.is_some() {
            sql.push_str(" WHERE s.deck_id = ?1");
        }
        sql.push_str(" ORDER BY s.started_at DESC, s.game_id DESC");
        if filter.limit.is_some() {
            if filter.deck_id.is_some() {
                sql.push_str(" LIMIT ?2");
            } else {
                sql.push_str(" LIMIT ?1");
            }
        }

        let mut stmt = conn.prepare(&sql)?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<GameRecord> {
            Ok(GameRecord {
                game_id: GameId(row.get::<_, i64>(0)? as u64),
                deck_id: row.get::<_, Option<i64>>(1)?.map(DeckId),
                started_at: row.get(2)?,
                ended_at: row.get(3)?,
                raw_log_path: row.get(4)?,
                raw_log_sha256: row.get(5)?,
                friendly_class: row.get(6)?,
                opposing_class: row.get(7)?,
                friendly_player: row.get::<_, Option<i32>>(8)?.map(PlayerId),
                winner: row.get::<_, Option<i32>>(9)?.map(PlayerId),
            })
        };

        let rows = match (filter.deck_id, filter.limit) {
            (Some(deck), Some(limit)) => stmt
                .query_map(params![deck.0, limit], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (Some(deck), None) => stmt
                .query_map(params![deck.0], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, Some(limit)) => stmt
                .query_map(params![limit], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            (None, None) => stmt
                .query_map([], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    fn deck_stats(&self) -> Result<Vec<DeckStats>, DecksError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT d.id, d.name,
                    COUNT(s.game_id) AS games,
                    SUM(CASE
                        WHEN r.winner IS NOT NULL AND f.player_id IS NOT NULL
                             AND r.winner = f.player_id THEN 1
                        ELSE 0 END) AS wins,
                    SUM(CASE
                        WHEN r.winner IS NOT NULL AND f.player_id IS NOT NULL
                             AND r.winner != f.player_id THEN 1
                        ELSE 0 END) AS losses
             FROM decks d
             INNER JOIN game_sessions s ON s.deck_id = d.id AND s.ended_at IS NOT NULL
             LEFT JOIN game_results r ON r.game_id = s.game_id
             LEFT JOIN game_players f ON f.game_id = s.game_id AND f.role = 'friendly'
             GROUP BY d.id
             ORDER BY d.id ASC",
        )?;
        let stats = stmt
            .query_map([], |row| {
                Ok(DeckStats {
                    deck_id: DeckId(row.get(0)?),
                    deck_name: row.get(1)?,
                    games: row.get::<_, i64>(2)? as u32,
                    wins: row.get::<_, i64>(3)? as u32,
                    losses: row.get::<_, i64>(4)? as u32,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(stats)
    }
}

fn insert_deck(
    tx: &rusqlite::Transaction<'_>,
    draft: &DeckDraft,
    activate: bool,
) -> Result<DeckId, DecksError> {
    let canonical = encode(&draft.list)?;
    if activate {
        tx.execute("UPDATE decks SET is_active = 0 WHERE is_active = 1", [])?;
    }
    tx.execute(
        "INSERT INTO decks (
             name, class, format, archived, is_active, canonical_deckstring,
             created_at, updated_at
         ) VALUES (?1, ?2, ?3, 0, ?4, ?5, datetime('now'), datetime('now'))",
        params![
            draft.name,
            draft.class,
            draft.list.format.to_raw(),
            i32::from(activate),
            canonical,
        ],
    )?;
    let id = DeckId(tx.last_insert_rowid());
    insert_version(tx, id, &canonical, &draft.list)?;
    Ok(id)
}

fn insert_version(
    tx: &rusqlite::Transaction<'_>,
    deck_id: DeckId,
    canonical: &str,
    list: &DeckList,
) -> Result<(), DecksError> {
    let next: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM deck_versions WHERE deck_id = ?1",
        [deck_id.0],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO deck_versions (deck_id, version, deckstring, created_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![deck_id.0, next, canonical],
    )?;
    let version_id = tx.last_insert_rowid();
    for hero in &list.heroes {
        tx.execute(
            "INSERT INTO deck_cards (version_id, slot, dbf_id, count, sideboard_owner)
             VALUES (?1, 'hero', ?2, 1, 0)",
            params![version_id, hero.0],
        )?;
    }
    for (card, count) in &list.cards {
        tx.execute(
            "INSERT INTO deck_cards (version_id, slot, dbf_id, count, sideboard_owner)
             VALUES (?1, 'main', ?2, ?3, 0)",
            params![version_id, card.0, *count as i64],
        )?;
    }
    for (owner, cards) in &list.sideboards {
        for (card, count) in cards {
            tx.execute(
                "INSERT INTO deck_cards (version_id, slot, dbf_id, count, sideboard_owner)
                 VALUES (?1, 'sideboard', ?2, ?3, ?4)",
                params![version_id, card.0, *count as i64, owner.0],
            )?;
        }
    }
    Ok(())
}

fn read_deck(conn: &Connection, id: DeckId) -> Result<Deck, DecksError> {
    let row = conn
        .query_row(
            "SELECT name, class, format, archived, canonical_deckstring, created_at, updated_at
             FROM decks WHERE id = ?1",
            [id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or(DecksError::NotFound(id))?;
    let (name, class, format_raw, archived, canonical, created_at, updated_at) = row;
    let list = decode(&canonical)?;
    Ok(Deck {
        id,
        name,
        class,
        format: FormatType::from_raw(format_raw),
        list,
        archived,
        created_at,
        updated_at,
    })
}

fn upsert_player(
    tx: &rusqlite::Transaction<'_>,
    game_id: i64,
    role: &str,
    player_id: Option<PlayerId>,
    class: Option<&str>,
) -> Result<(), DecksError> {
    tx.execute(
        "INSERT INTO game_players (game_id, role, player_id, class)
         VALUES (?1, ?2, ?3, ?4)",
        params![game_id, role, player_id.map(|p| p.0), class],
    )?;
    Ok(())
}

fn opposing_player(friendly: Option<PlayerId>) -> Option<PlayerId> {
    match friendly {
        Some(PlayerId(1)) => Some(PlayerId(2)),
        Some(PlayerId(2)) => Some(PlayerId(1)),
        _ => None,
    }
}

fn game_id_sql(id: GameId) -> Result<i64, DecksError> {
    i64::try_from(id.0).map_err(|_| DecksError::Invalid(format!("game_id {} exceeds i64", id.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_power_log;
    use hs_types::DbfId;

    const CANONICAL: &str = "AAEBAQcAAAQBAwIDAwMEAwA=";

    fn sample_list() -> DeckList {
        decode(CANONICAL).unwrap()
    }

    fn sample_draft(name: &str) -> DeckDraft {
        DeckDraft::new(name, "WARRIOR", sample_list())
    }

    fn game(game_id: u64, deck_id: Option<DeckId>, winner: Option<i32>, ended: bool) -> GameRecord {
        GameRecord {
            game_id: GameId(game_id),
            deck_id,
            friendly_class: Some("WARRIOR".into()),
            opposing_class: Some("MAGE".into()),
            friendly_player: Some(PlayerId(1)),
            winner: winner.map(PlayerId),
            started_at: format!("2026-08-21 10:00:{game_id:02}"),
            ended_at: ended.then(|| format!("2026-08-21 10:10:{game_id:02}")),
            raw_log_path: None,
            raw_log_sha256: None,
        }
    }

    #[test]
    fn crud_copy_archive_and_active() {
        let store = SqliteDeckStore::open_in_memory().unwrap();
        let created = store.create(&sample_draft("核心")).unwrap();
        assert_eq!(created.name, "核心");
        assert_eq!(created.class, "WARRIOR");
        assert_eq!(created.list, sample_list());
        assert!(!created.archived);
        assert!(store.active().unwrap().is_none());

        store.set_active(Some(created.id)).unwrap();
        assert_eq!(store.active().unwrap().unwrap().id, created.id);

        let copied = store.copy(created.id, "核心 副本").unwrap();
        assert_ne!(copied.id, created.id);
        assert_eq!(copied.list, created.list);
        assert_eq!(copied.name, "核心 副本");

        let updated_list = DeckList {
            format: FormatType::Standard,
            heroes: vec![DbfId(7)],
            cards: vec![(DbfId(1), 1)],
            sideboards: vec![],
        };
        let updated = store
            .update(
                copied.id,
                &DeckDraft::new("核心改", "WARRIOR", updated_list.clone()),
            )
            .unwrap();
        assert_eq!(updated.list, updated_list);
        assert_eq!(updated.name, "核心改");

        store.set_archived(created.id, true).unwrap();
        assert!(store.get(created.id).unwrap().archived);
        assert!(store.active().unwrap().is_none());
        assert_eq!(store.list(false).unwrap().len(), 1);
        assert_eq!(store.list(true).unwrap().len(), 2);

        store.set_active(None).unwrap();
        assert!(store.active().unwrap().is_none());
        assert!(matches!(
            store.get(DeckId(999)),
            Err(DecksError::NotFound(DeckId(999)))
        ));
    }

    #[test]
    fn ingest_creates_then_is_idempotent() {
        let store = SqliteDeckStore::open_in_memory().unwrap();
        let first = store
            .ingest_observed_deck("天梯战士", "AAEBAQcAAAQBAwIDAwMEAw==")
            .unwrap();
        let again = store
            .ingest_observed_deck("另一个名字", &format!("### x\n{CANONICAL}\n"))
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(store.list(true).unwrap().len(), 1);
        let active = store.active().unwrap().unwrap();
        assert_eq!(active.id, first);
        assert_eq!(active.name, "天梯战士");

        store.set_archived(first, true).unwrap();
        let revived = store.ingest_observed_deck("忽略", CANONICAL).unwrap();
        assert_eq!(revived, first);
        assert!(!store.get(first).unwrap().archived);
        assert_eq!(store.active().unwrap().unwrap().id, first);
    }

    #[test]
    fn upsert_does_not_change_active() {
        let store = SqliteDeckStore::open_in_memory().unwrap();
        let first = store
            .ingest_observed_deck("排队", "AAEBAQcAAAQBAwIDAwMEAw==")
            .unwrap();
        let other = encode(&DeckList {
            format: FormatType::Standard,
            heroes: vec![DbfId(8)],
            cards: vec![(DbfId(2), 2)],
            sideboards: vec![],
        })
        .unwrap();
        let extra = store.upsert_observed_deck("收藏", &other).unwrap();
        assert_ne!(first, extra);
        assert_eq!(store.active().unwrap().unwrap().id, first);
        let again = store.upsert_observed_deck("收藏改名", &other).unwrap();
        assert_eq!(again, extra);
        assert_eq!(store.active().unwrap().unwrap().id, first);
    }

    #[test]
    fn game_history_upsert_and_stats() {
        let store = SqliteDeckStore::open_in_memory().unwrap();
        let deck = store.create(&sample_draft("统计")).unwrap();
        store
            .record_game(&game(1, Some(deck.id), Some(1), true))
            .unwrap();
        store
            .record_game(&game(2, Some(deck.id), Some(1), true))
            .unwrap();
        store
            .record_game(&game(3, Some(deck.id), Some(2), true))
            .unwrap();
        store
            .record_game(&game(4, Some(deck.id), None, false))
            .unwrap();

        let mut ended = game(1, Some(deck.id), Some(2), true);
        ended.raw_log_path = Some("data/games/1.power.log".into());
        ended.raw_log_sha256 = Some("abc".into());
        store.record_game(&ended).unwrap();

        let all = store.list_games(&GameFilter::default()).unwrap();
        assert_eq!(all.len(), 4);
        let first = all.iter().find(|g| g.game_id == GameId(1)).unwrap();
        assert_eq!(first.winner, Some(PlayerId(2)));
        assert_eq!(first.raw_log_sha256.as_deref(), Some("abc"));

        let filtered = store
            .list_games(&GameFilter {
                deck_id: Some(deck.id),
                limit: Some(2),
            })
            .unwrap();
        assert_eq!(filtered.len(), 2);

        let stats = store.deck_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].deck_id, deck.id);
        assert_eq!(stats[0].games, 3);
        assert_eq!(stats[0].wins, 1);
        assert_eq!(stats[0].losses, 2);
    }

    #[test]
    fn archive_helper_roundtrip_bytes() {
        let dir =
            std::env::temp_dir().join(format!("hs-decks-store-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let archived = archive_power_log(&dir, GameId(9), "hello").unwrap();
        assert!(archived.path.exists());
        assert_eq!(std::fs::read_to_string(&archived.path).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
