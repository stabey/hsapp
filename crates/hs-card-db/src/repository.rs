//! SQLite 实现的 `CardRepository`（TB-01）：查询 + 单事务 upsert 写入。

use crate::migrate::{apply_migrations, BUILTIN_MIGRATIONS};
use crate::{AssetKey, CardDbError, CardDefinition, CardRepository};
use hs_types::{BuildId, CardId};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// 一次导入写入的卡牌行（含投影字段、原文与全部 locale）。
///
/// 与 [`CardDefinition`] 分离：后者是 `#[non_exhaustive]` 查询投影，
/// 外部 crate 不能用结构体字面量构造。
#[derive(Clone, Debug)]
pub struct CardRecord {
    pub build_id: BuildId,
    pub card_id: CardId,
    pub dbf_id: Option<i64>,
    pub card_type: Option<String>,
    pub card_set: Option<String>,
    pub rarity: Option<String>,
    pub card_class_json: Option<String>,
    pub cost: Option<i32>,
    pub attack: Option<i32>,
    pub health: Option<i32>,
    pub collectible: bool,
    pub raw_json: String,
    pub localizations: Vec<CardLocalization>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardLocalization {
    pub locale: String,
    pub name: Option<String>,
    pub text: Option<String>,
    pub flavor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameBuildRecord {
    pub build_id: BuildId,
    pub version_text: Option<String>,
    pub source_manifest: String,
    pub source_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportStats {
    pub cards_upserted: usize,
    pub localizations_upserted: usize,
}

/// 池查询过滤条件(全部可选,AND 语义;字段语义与 hs-pools::CardFilter 对齐,
/// 本 crate 不依赖 hs-pools 故独立定义)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoolCardFilter {
    pub collectible: Option<bool>,
    pub card_type: Option<String>,
    /// 职业码集合(任一匹配;卡无职业按 NEUTRAL 参与匹配)。
    pub card_classes: Vec<String>,
    pub rarity: Option<String>,
    pub card_sets: Vec<String>,
    pub cost_min: Option<i32>,
    pub cost_max: Option<i32>,
}

/// 池查询结果行(hs-pools::CardMeta 的数据源投影)。
#[derive(Clone, Debug, PartialEq)]
pub struct PoolCardRow {
    pub card_id: CardId,
    pub dbf_id: Option<i64>,
    pub card_type: Option<String>,
    pub card_set: Option<String>,
    pub rarity: Option<String>,
    pub card_class: Option<String>,
    pub cost: Option<i32>,
    pub collectible: bool,
}

/// `card_assets` 一行（下载链路读写；不改 schema）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardAssetRow {
    pub local_path: Option<String>,
    pub remote_etag: Option<String>,
    pub sha256: Option<String>,
    pub status: String,
}

/// `["MAGE"]` / `"MAGE"` → `MAGE`。空或无法识别返回 None。
fn parse_card_class_json(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    match value {
        serde_json::Value::String(s) if !s.is_empty() => Some(s),
        serde_json::Value::Array(items) => items.into_iter().find_map(|item| match item {
            serde_json::Value::String(s) if !s.is_empty() && s != "NEUTRAL" => Some(s),
            _ => None,
        }),
        _ => None,
    }
}

/// 文件或内存 SQLite 卡牌库。查询走 [`CardRepository`]；写入走固有方法。
pub struct SqliteCardRepository {
    conn: Connection,
}

impl SqliteCardRepository {
    pub fn open_in_memory() -> Result<Self, CardDbError> {
        let mut conn = Connection::open_in_memory()?;
        apply_migrations(&mut conn, BUILTIN_MIGRATIONS)?;
        Ok(Self { conn })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, CardDbError> {
        let mut conn = Connection::open(path)?;
        apply_migrations(&mut conn, BUILTIN_MIGRATIONS)?;
        Ok(Self { conn })
    }

    /// 最近一次导入的 build（`is_current = 1`）。空库返回 None。
    ///
    /// UI 展示卡牌需要一个默认 build；历史录像应改用录像自带的 build（§9.2）。
    pub fn current_build(&self) -> Result<Option<BuildId>, CardDbError> {
        let build = self
            .conn
            .query_row(
                "SELECT build_id FROM game_builds WHERE is_current = 1 LIMIT 1",
                [],
                |row| row.get::<_, u32>(0).map(BuildId),
            )
            .optional()?;
        Ok(build)
    }

    /// 卡牌职业(`card_class_json` 如 `["MAGE"]` → `MAGE`);没有或解析失败为 None。
    pub fn card_class(
        &self,
        build: BuildId,
        card_id: &CardId,
    ) -> Result<Option<String>, CardDbError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT card_class_json FROM card_definitions
                 WHERE build_id = ?1 AND card_id = ?2",
                params![build.0, card_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(json.as_deref().and_then(parse_card_class_json))
    }

    /// 按 `dbf_id` 取本地化名（牌组是 dbf 表示，日志是 card_id 表示）。
    /// 返回 `(card_id, name)`；`name` 缺该 locale 时为 None。
    pub fn card_by_dbf(
        &self,
        build: BuildId,
        dbf_id: i64,
        locale: &str,
    ) -> Result<Option<(CardId, Option<String>)>, CardDbError> {
        let found = self
            .conn
            .query_row(
                "SELECT d.card_id, l.name
                 FROM card_definitions d
                 LEFT JOIN card_localizations l
                     ON l.build_id = d.build_id AND l.card_id = d.card_id AND l.locale = ?3
                 WHERE d.build_id = ?1 AND d.dbf_id = ?2",
                params![build.0, dbf_id, locale],
                |row| {
                    Ok((
                        CardId::new(row.get::<_, String>(0)?),
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        Ok(found)
    }

    /// 池查询(M3 Track D):按可选条件过滤卡牌定义,结果按 card_id 升序。
    ///
    /// 供 desktop 侧适配 `hs-pools::CardQuery` 用;本 crate 不依赖 hs-pools,
    /// 过滤参数用本地结构 [`PoolCardFilter`] 表达。职业过滤在 Rust 侧做
    /// (card_class_json 为 JSON 数组,SQL 内解析不可移植)。
    pub fn cards_for_pool(
        &self,
        build: BuildId,
        filter: &PoolCardFilter,
    ) -> Result<Vec<PoolCardRow>, CardDbError> {
        let mut sql = String::from(
            "SELECT card_id, dbf_id, card_type, card_set, rarity, card_class_json, cost, collectible
             FROM card_definitions WHERE build_id = ?1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(build.0)];
        if let Some(collectible) = filter.collectible {
            args.push(Box::new(i32::from(collectible)));
            sql.push_str(&format!(" AND collectible = ?{}", args.len()));
        }
        if let Some(card_type) = &filter.card_type {
            args.push(Box::new(card_type.clone()));
            sql.push_str(&format!(" AND card_type = ?{}", args.len()));
        }
        if let Some(rarity) = &filter.rarity {
            args.push(Box::new(rarity.clone()));
            sql.push_str(&format!(" AND rarity = ?{}", args.len()));
        }
        if !filter.card_sets.is_empty() {
            let mut placeholders = Vec::with_capacity(filter.card_sets.len());
            for set in &filter.card_sets {
                args.push(Box::new(set.clone()));
                placeholders.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND card_set IN ({})", placeholders.join(",")));
        }
        if let Some(cost_min) = filter.cost_min {
            args.push(Box::new(cost_min));
            sql.push_str(&format!(" AND cost >= ?{}", args.len()));
        }
        if let Some(cost_max) = filter.cost_max {
            args.push(Box::new(cost_max));
            sql.push_str(&format!(" AND cost <= ?{}", args.len()));
        }
        sql.push_str(" ORDER BY card_id ASC");

        let mut stmt = self.conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(AsRef::as_ref).collect();
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            Ok(PoolCardRow {
                card_id: CardId::new(row.get::<_, String>(0)?),
                dbf_id: row.get(1)?,
                card_type: row.get(2)?,
                card_set: row.get(3)?,
                rarity: row.get(4)?,
                card_class: row
                    .get::<_, Option<String>>(5)?
                    .as_deref()
                    .and_then(parse_card_class_json),
                cost: row.get(6)?,
                collectible: row.get::<_, i32>(7)? != 0,
            })
        })?;
        let mut cards = Vec::new();
        for row in rows {
            let card = row?;
            if !filter.card_classes.is_empty() {
                let class = card.card_class.as_deref().unwrap_or("NEUTRAL");
                if !filter.card_classes.iter().any(|c| c == class) {
                    continue;
                }
            }
            cards.push(card);
        }
        Ok(cards)
    }

    /// 同一事务内 upsert `game_builds` + 全部卡牌定义与本地化（重复导入幂等）。
    pub fn import_build(
        &mut self,
        build: &GameBuildRecord,
        cards: &[CardRecord],
    ) -> Result<ImportStats, CardDbError> {
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE game_builds SET is_current = 0", [])?;
        tx.execute(
            "INSERT INTO game_builds (
                 build_id, version_text, imported_at, source_manifest, source_hash, is_current
             ) VALUES (?1, ?2, datetime('now'), ?3, ?4, 1)
             ON CONFLICT(build_id) DO UPDATE SET
                 version_text = excluded.version_text,
                 imported_at = datetime('now'),
                 source_manifest = excluded.source_manifest,
                 source_hash = excluded.source_hash,
                 is_current = 1",
            params![
                build.build_id.0,
                build.version_text,
                build.source_manifest,
                build.source_hash,
            ],
        )?;

        let mut cards_upserted = 0usize;
        let mut localizations_upserted = 0usize;
        for card in cards {
            tx.execute(
                "INSERT INTO card_definitions (
                     build_id, card_id, dbf_id, card_type, card_set, rarity,
                     card_class_json, cost, attack, health, collectible, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(build_id, card_id) DO UPDATE SET
                     dbf_id = excluded.dbf_id,
                     card_type = excluded.card_type,
                     card_set = excluded.card_set,
                     rarity = excluded.rarity,
                     card_class_json = excluded.card_class_json,
                     cost = excluded.cost,
                     attack = excluded.attack,
                     health = excluded.health,
                     collectible = excluded.collectible,
                     raw_json = excluded.raw_json",
                params![
                    build.build_id.0,
                    card.card_id.as_str(),
                    card.dbf_id,
                    card.card_type,
                    card.card_set,
                    card.rarity,
                    card.card_class_json,
                    card.cost,
                    card.attack,
                    card.health,
                    i32::from(card.collectible),
                    card.raw_json,
                ],
            )?;
            cards_upserted += 1;

            for loc in &card.localizations {
                tx.execute(
                    "INSERT INTO card_localizations (
                         build_id, card_id, locale, name, text, flavor
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(build_id, card_id, locale) DO UPDATE SET
                         name = excluded.name,
                         text = excluded.text,
                         flavor = excluded.flavor",
                    params![
                        build.build_id.0,
                        card.card_id.as_str(),
                        loc.locale,
                        loc.name,
                        loc.text,
                        loc.flavor,
                    ],
                )?;
                localizations_upserted += 1;
            }
        }

        tx.commit()?;
        Ok(ImportStats {
            cards_upserted,
            localizations_upserted,
        })
    }

    /// 写入或覆盖一条卡图元数据（`card_assets` 已有，不新增列）。
    pub fn upsert_card_asset(
        &self,
        key: &AssetKey,
        local_path: Option<&str>,
        remote_etag: Option<&str>,
        sha256: Option<&str>,
        status: &str,
    ) -> Result<(), CardDbError> {
        self.conn.execute(
            "INSERT INTO card_assets (
                 build_id, card_id, locale, asset_kind, resolution,
                 local_path, remote_etag, sha256, status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(build_id, card_id, locale, asset_kind, resolution)
             DO UPDATE SET
                 local_path = excluded.local_path,
                 remote_etag = excluded.remote_etag,
                 sha256 = excluded.sha256,
                 status = excluded.status",
            params![
                key.build.0,
                key.card_id.as_str(),
                key.locale,
                key.asset_kind,
                key.resolution,
                local_path,
                remote_etag,
                sha256,
                status,
            ],
        )?;
        Ok(())
    }

    pub fn card_asset(&self, key: &AssetKey) -> Result<Option<CardAssetRow>, CardDbError> {
        let row = self
            .conn
            .query_row(
                "SELECT local_path, remote_etag, sha256, status
                 FROM card_assets
                 WHERE build_id = ?1 AND card_id = ?2 AND locale = ?3
                   AND asset_kind = ?4 AND resolution = ?5",
                params![
                    key.build.0,
                    key.card_id.as_str(),
                    key.locale,
                    key.asset_kind,
                    key.resolution,
                ],
                |row| {
                    Ok(CardAssetRow {
                        local_path: row.get(0)?,
                        remote_etag: row.get(1)?,
                        sha256: row.get(2)?,
                        status: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}

impl CardRepository for SqliteCardRepository {
    fn list_builds(&self) -> Result<Vec<BuildId>, CardDbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT build_id FROM game_builds ORDER BY build_id ASC")?;
        let builds = stmt
            .query_map([], |row| row.get::<_, u32>(0).map(BuildId))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(builds)
    }

    fn card(&self, build: BuildId, card_id: &CardId) -> Result<CardDefinition, CardDbError> {
        let found = self
            .conn
            .query_row(
                "SELECT build_id, card_id, dbf_id, card_type, cost, attack, health, collectible
                 FROM card_definitions
                 WHERE build_id = ?1 AND card_id = ?2",
                params![build.0, card_id.as_str()],
                |row| {
                    Ok(CardDefinition {
                        build_id: BuildId(row.get(0)?),
                        card_id: CardId::new(row.get::<_, String>(1)?),
                        dbf_id: row.get(2)?,
                        card_type: row.get(3)?,
                        cost: row.get(4)?,
                        attack: row.get(5)?,
                        health: row.get(6)?,
                        collectible: row.get::<_, i64>(7)? != 0,
                    })
                },
            )
            .optional()?;
        found.ok_or_else(|| CardDbError::NotFound {
            build,
            card: card_id.clone(),
        })
    }

    fn card_name(
        &self,
        build: BuildId,
        card_id: &CardId,
        locale: &str,
    ) -> Result<Option<String>, CardDbError> {
        let name = self
            .conn
            .query_row(
                "SELECT name FROM card_localizations
                 WHERE build_id = ?1 AND card_id = ?2 AND locale = ?3",
                params![build.0, card_id.as_str(), locale],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(name.flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_card_class_json_takes_first_non_neutral() {
        assert_eq!(
            parse_card_class_json(r#"["MAGE"]"#).as_deref(),
            Some("MAGE")
        );
        assert_eq!(
            parse_card_class_json(r#"["NEUTRAL","PRIEST"]"#).as_deref(),
            Some("PRIEST")
        );
        assert_eq!(
            parse_card_class_json(r#""WARRIOR""#).as_deref(),
            Some("WARRIOR")
        );
        assert_eq!(parse_card_class_json("[]"), None);
        assert_eq!(parse_card_class_json("not-json"), None);
    }

    fn fireball(build: BuildId) -> CardRecord {
        CardRecord {
            build_id: build,
            card_id: CardId::new("CS2_029"),
            dbf_id: Some(315),
            card_type: Some("SPELL".into()),
            card_set: Some("CORE".into()),
            rarity: Some("COMMON".into()),
            card_class_json: Some(r#"["MAGE"]"#.into()),
            cost: Some(4),
            attack: None,
            health: None,
            collectible: true,
            raw_json: r#"{"id":"CS2_029","name":{"zhCN":"火球术"}}"#.into(),
            localizations: vec![
                CardLocalization {
                    locale: "zhCN".into(),
                    name: Some("火球术".into()),
                    text: Some("造成$6点伤害。".into()),
                    flavor: None,
                },
                CardLocalization {
                    locale: "enUS".into(),
                    name: Some("Fireball".into()),
                    text: Some("Deal $6 damage.".into()),
                    flavor: None,
                },
            ],
        }
    }

    fn seed(repo: &mut SqliteCardRepository, build: BuildId) -> ImportStats {
        repo.import_build(
            &GameBuildRecord {
                build_id: build,
                version_text: Some("test".into()),
                source_manifest: "memory://test".into(),
                source_hash: "abc".into(),
            },
            &[fireball(build)],
        )
        .unwrap()
    }

    fn count_defs(repo: &SqliteCardRepository, build: BuildId) -> i64 {
        repo.conn
            .query_row(
                "SELECT COUNT(*) FROM card_definitions WHERE build_id = ?1",
                params![build.0],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn count_locs(repo: &SqliteCardRepository, build: BuildId) -> i64 {
        repo.conn
            .query_row(
                "SELECT COUNT(*) FROM card_localizations WHERE build_id = ?1",
                params![build.0],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn empty_repo_lists_no_builds() {
        let repo = SqliteCardRepository::open_in_memory().unwrap();
        assert!(repo.list_builds().unwrap().is_empty());
    }

    #[test]
    fn import_then_query_definition_and_names() {
        let mut repo = SqliteCardRepository::open_in_memory().unwrap();
        let build = BuildId(999999);
        seed(&mut repo, build);

        assert_eq!(repo.list_builds().unwrap(), vec![build]);

        let card = repo.card(build, &CardId::new("CS2_029")).unwrap();
        assert_eq!(card.card_id, CardId::new("CS2_029"));
        assert_eq!(card.dbf_id, Some(315));
        assert_eq!(card.card_type.as_deref(), Some("SPELL"));
        assert_eq!(card.cost, Some(4));
        assert!(card.collectible);

        assert_eq!(
            repo.card_class(build, &CardId::new("CS2_029"))
                .unwrap()
                .as_deref(),
            Some("MAGE")
        );
        assert_eq!(
            repo.card_name(build, &CardId::new("CS2_029"), "zhCN")
                .unwrap()
                .as_deref(),
            Some("火球术")
        );
        assert_eq!(
            repo.card_name(build, &CardId::new("CS2_029"), "enUS")
                .unwrap()
                .as_deref(),
            Some("Fireball")
        );
        assert_eq!(
            repo.card_name(build, &CardId::new("CS2_029"), "deDE")
                .unwrap(),
            None
        );
    }

    #[test]
    fn missing_card_is_not_found() {
        let repo = SqliteCardRepository::open_in_memory().unwrap();
        let err = repo.card(BuildId(1), &CardId::new("MISSING")).unwrap_err();
        assert!(matches!(
            err,
            CardDbError::NotFound {
                build: BuildId(1),
                ..
            }
        ));
    }

    #[test]
    fn reimport_same_build_is_idempotent_and_updates() {
        let mut repo = SqliteCardRepository::open_in_memory().unwrap();
        let build = BuildId(42);
        let first = seed(&mut repo, build);
        assert_eq!(first.cards_upserted, 1);
        assert_eq!(first.localizations_upserted, 2);
        assert_eq!(count_defs(&repo, build), 1);
        assert_eq!(count_locs(&repo, build), 2);

        let mut updated = fireball(build);
        updated.cost = Some(5);
        updated.localizations[0].name = Some("火球术".into());
        repo.import_build(
            &GameBuildRecord {
                build_id: build,
                version_text: Some("test-2".into()),
                source_manifest: "memory://test".into(),
                source_hash: "def".into(),
            },
            &[updated],
        )
        .unwrap();

        assert_eq!(count_defs(&repo, build), 1);
        assert_eq!(count_locs(&repo, build), 2);
        assert_eq!(
            repo.card(build, &CardId::new("CS2_029")).unwrap().cost,
            Some(5)
        );
        assert_eq!(repo.list_builds().unwrap(), vec![build]);
    }

    #[test]
    fn upsert_card_asset_round_trip() {
        let repo = SqliteCardRepository::open_in_memory().unwrap();
        let key = crate::AssetKey {
            build: BuildId(1),
            card_id: CardId::new("CS2_029"),
            locale: "zhCN".into(),
            asset_kind: "tile".into(),
            resolution: "256".into(),
        };
        repo.upsert_card_asset(&key, Some("/tmp/a"), Some("etag"), Some("abc"), "ready")
            .unwrap();
        let row = repo.card_asset(&key).unwrap().unwrap();
        assert_eq!(row.status, "ready");
        assert_eq!(row.local_path.as_deref(), Some("/tmp/a"));
        assert_eq!(row.sha256.as_deref(), Some("abc"));

        repo.upsert_card_asset(&key, None, None, None, "failed")
            .unwrap();
        let row = repo.card_asset(&key).unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert!(row.local_path.is_none());
    }

    #[test]
    fn builds_are_isolated() {
        let mut repo = SqliteCardRepository::open_in_memory().unwrap();
        seed(&mut repo, BuildId(1));
        seed(&mut repo, BuildId(2));
        assert_eq!(repo.list_builds().unwrap(), vec![BuildId(1), BuildId(2)]);
        assert!(repo.card(BuildId(1), &CardId::new("CS2_029")).is_ok());
        assert!(repo.card(BuildId(2), &CardId::new("CS2_029")).is_ok());
        assert!(matches!(
            repo.card(BuildId(3), &CardId::new("CS2_029")),
            Err(CardDbError::NotFound { .. })
        ));
    }
}
