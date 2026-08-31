//! 卡牌显示名解析(M2 Track D)。
//!
//! 牌组用 `dbf_id`、日志用 `card_id`,UI 两边都要显示中文名——这个映射是
//! UI 层的活(设计文档 §6.1:派生层不做 dbf↔card 映射)。
//! 查不到一律降级为原始 ID,断网/空库不影响追踪(PRD 决策 1)。

use gpui::Global;
use hs_card_db::{CardRepository, SqliteCardRepository};
use hs_types::{BuildId, CardId, DbfId};
use std::cell::RefCell;
use std::collections::HashMap;

/// 显示用 locale。跟随 fetch 导入的主 locale(M2 固定简中)。
pub const DISPLAY_LOCALE: &str = "zhCN";

pub struct CardNames {
    repo: Option<SqliteCardRepository>,
    build: Option<BuildId>,
    /// 每帧都可能查同一张卡,必须缓存;None = 库里没有。
    by_card_id: RefCell<HashMap<CardId, Option<String>>>,
    by_dbf: RefCell<HashMap<DbfId, Option<String>>>,
    pub open_error: Option<String>,
}

impl Global for CardNames {}

impl CardNames {
    pub fn open_default() -> Self {
        let path = hs_platform::default_card_db_path();
        match SqliteCardRepository::open(&path) {
            Ok(repo) => {
                let build = repo.current_build().ok().flatten();
                Self {
                    repo: Some(repo),
                    build,
                    by_card_id: RefCell::new(HashMap::new()),
                    by_dbf: RefCell::new(HashMap::new()),
                    open_error: None,
                }
            }
            Err(err) => Self {
                repo: None,
                build: None,
                by_card_id: RefCell::new(HashMap::new()),
                by_dbf: RefCell::new(HashMap::new()),
                open_error: Some(format!("打开卡牌库 {} 失败: {err}", path.display())),
            },
        }
    }

    /// 卡牌库是否可用(已导入过卡牌)。
    pub fn is_ready(&self) -> bool {
        self.repo.is_some() && self.build.is_some()
    }

    /// `card_id` → 显示名;查不到返回 `card_id` 原文。
    pub fn name_of_card(&self, card_id: &CardId) -> String {
        let fallback = || card_id.as_str().to_string();
        let (Some(repo), Some(build)) = (&self.repo, self.build) else {
            return fallback();
        };
        if let Some(cached) = self.by_card_id.borrow().get(card_id) {
            return cached.clone().unwrap_or_else(fallback);
        }
        let name = repo
            .card_name(build, card_id, DISPLAY_LOCALE)
            .ok()
            .flatten();
        self.by_card_id
            .borrow_mut()
            .insert(card_id.clone(), name.clone());
        name.unwrap_or_else(fallback)
    }

    /// `dbf_id` → 显示名;查不到返回 `dbf:<id>`。
    pub fn name_of_dbf(&self, dbf: DbfId) -> String {
        let fallback = || format!("dbf:{}", dbf.0);
        let (Some(repo), Some(build)) = (&self.repo, self.build) else {
            return fallback();
        };
        if let Some(cached) = self.by_dbf.borrow().get(&dbf) {
            return cached.clone().unwrap_or_else(fallback);
        }
        let name = repo
            .card_by_dbf(build, dbf.0, DISPLAY_LOCALE)
            .ok()
            .flatten()
            .and_then(|(_, name)| name);
        self.by_dbf.borrow_mut().insert(dbf, name.clone());
        name.unwrap_or_else(fallback)
    }

    /// 英雄卡 → CardClass 名(如 `MAGE`);查不到返回 None。
    pub fn class_of_card(&self, card_id: &CardId) -> Option<String> {
        let (repo, build) = self.repo.as_ref().zip(self.build)?;
        repo.card_class(build, card_id).ok().flatten()
    }
}

/// 牌库行的显示名:优先 card_id(日志观察到的),否则牌组的 dbf_id。
pub fn row_label(cx: &gpui::App, card_id: Option<&CardId>, dbf: Option<DbfId>) -> String {
    let names = cx.global::<CardNames>();
    match (card_id, dbf) {
        (Some(card), _) => names.name_of_card(card),
        (None, Some(dbf)) => names.name_of_dbf(dbf),
        (None, None) => "?".to_string(),
    }
}
