//! 轻量 SQLite Migration Runner（G0-05，方案决策见任务 design.md §3 与 ADR-0004）。
//!
//! 规则：
//! - 迁移按版本号严格升序应用，每个迁移一个事务；
//! - `schema_migrations` 记录已应用版本，重复运行幂等；
//! - 禁止降级/丢表重建（设计文档 §17.1）。

use crate::CardDbError;
use rusqlite::Connection;

/// 编译期内嵌的全部迁移（版本号, SQL）。新增迁移在此追加，禁止改历史条目。
pub const BUILTIN_MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        include_str!("../../../data/migrations/0001_game_builds.sql"),
    ),
    (
        2,
        include_str!("../../../data/migrations/0002_card_definitions.sql"),
    ),
    (
        3,
        include_str!("../../../data/migrations/0003_card_localizations.sql"),
    ),
    (
        4,
        include_str!("../../../data/migrations/0004_card_assets.sql"),
    ),
];

/// 应用所有未执行的迁移，返回本次实际应用的版本列表。
pub fn apply_migrations(
    conn: &mut Connection,
    migrations: &[(u32, &str)],
) -> Result<Vec<u32>, CardDbError> {
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
            .map_err(|e| CardDbError::Migration(format!("v{version}: {e}")))?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, datetime('now'))",
            [version],
        )?;
        tx.commit()?;
        applied.push(version);
    }
    Ok(applied)
}

/// 当前已应用到的最大版本（未初始化时为 None）。
pub fn current_version(conn: &Connection) -> Result<Option<u32>, CardDbError> {
    let table_exists: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations'")?
        .exists([])?;
    if !table_exists {
        return Ok(None);
    }
    let version: Option<u32> =
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    Ok(version)
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
    fn applies_all_builtin_migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        let applied = apply_migrations(&mut conn, BUILTIN_MIGRATIONS).unwrap();
        assert_eq!(applied, vec![1, 2, 3, 4]);

        let tables = table_names(&conn);
        for t in [
            "game_builds",
            "card_definitions",
            "card_localizations",
            "card_assets",
        ] {
            assert!(tables.iter().any(|n| n == t), "missing table {t}");
        }
        assert_eq!(current_version(&conn).unwrap(), Some(4));
    }

    #[test]
    fn rerun_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        let first = apply_migrations(&mut conn, BUILTIN_MIGRATIONS).unwrap();
        assert_eq!(first.len(), 4);
        let second = apply_migrations(&mut conn, BUILTIN_MIGRATIONS).unwrap();
        assert!(second.is_empty(), "second run must apply nothing");
        assert_eq!(current_version(&conn).unwrap(), Some(4));
    }

    #[test]
    fn fresh_db_has_no_version() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(current_version(&conn).unwrap(), None);
    }

    #[test]
    fn partial_upgrade_applies_only_missing() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn, &BUILTIN_MIGRATIONS[..2]).unwrap();
        assert_eq!(current_version(&conn).unwrap(), Some(2));
        let applied = apply_migrations(&mut conn, BUILTIN_MIGRATIONS).unwrap();
        assert_eq!(applied, vec![3, 4]);
    }
}
