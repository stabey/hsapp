//! 牌组/对局业务表 migration runner(模式对齐 `hs-card-db::migrate`)。
//!
//! 只承载 0005–0010;0011+ 由其他 Track 申请。与卡牌表共用 `schema_migrations`,
//! 按版本号幂等,不要求 0001–0004 先于本 crate 应用。

use crate::DecksError;
use rusqlite::Connection;

/// 编译期内嵌的牌组/对局迁移(版本号, SQL)。禁止改历史条目。
pub const DECK_MIGRATIONS: &[(u32, &str)] = &[
    (5, include_str!("../../../data/migrations/0005_decks.sql")),
    (
        6,
        include_str!("../../../data/migrations/0006_deck_versions.sql"),
    ),
    (
        7,
        include_str!("../../../data/migrations/0007_deck_cards.sql"),
    ),
    (
        8,
        include_str!("../../../data/migrations/0008_game_sessions.sql"),
    ),
    (
        9,
        include_str!("../../../data/migrations/0009_game_players.sql"),
    ),
    (
        10,
        include_str!("../../../data/migrations/0010_game_results.sql"),
    ),
];

/// 应用所有未执行的 0005–0010 迁移,返回本次实际应用的版本列表。
pub fn apply_deck_migrations(conn: &mut Connection) -> Result<Vec<u32>, DecksError> {
    apply_migrations(conn, DECK_MIGRATIONS)
}

pub fn apply_migrations(
    conn: &mut Connection,
    migrations: &[(u32, &str)],
) -> Result<Vec<u32>, DecksError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             applied_at TEXT NOT NULL
         );",
    )?;

    let mut applied = Vec::new();
    for &(version, sql) in migrations {
        let exists: bool = conn
            .prepare("SELECT 1 FROM schema_migrations WHERE version = ?1")?
            .exists([version])?;
        if exists {
            continue;
        }

        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .map_err(|e| DecksError::Migration(format!("v{version}: {e}")))?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, datetime('now'))",
            [version],
        )?;
        tx.commit()?;
        applied.push(version);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn applies_all_deck_migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        let applied = apply_deck_migrations(&mut conn).unwrap();
        assert_eq!(applied, vec![5, 6, 7, 8, 9, 10]);
        let tables = table_names(&conn);
        for t in [
            "decks",
            "deck_versions",
            "deck_cards",
            "game_sessions",
            "game_players",
            "game_results",
        ] {
            assert!(tables.iter().any(|n| n == t), "missing table {t}");
        }
    }

    #[test]
    fn rerun_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(apply_deck_migrations(&mut conn).unwrap().len(), 6);
        assert!(apply_deck_migrations(&mut conn).unwrap().is_empty());
    }
}
