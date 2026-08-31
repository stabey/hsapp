//! 领域仓储全局句柄(M2 Track D)。
//!
//! 牌组与对局历史共用 `cards.sqlite` 一个库(migration 0005–0010 由
//! `hs-decks` 的 runner 补齐)。打开失败时保持 None,各页显示空态,
//! 不阻塞日志追踪与 Overlay(PRD 决策 1 的降级精神)。

use gpui::Global;
use hs_decks::{DeckRepository, GameHistoryRepository, SqliteDeckStore};
use hs_deckstrings::DeckList;
use std::rc::Rc;

/// 当前活动牌组 ID(对局落库时关联)。
pub fn active_deck_id(cx: &gpui::App) -> Option<hs_types::DeckId> {
    cx.global::<Stores>()
        .decks
        .as_ref()?
        .active()
        .ok()
        .flatten()
        .map(|deck| deck.id)
}

/// 当前活动牌组的牌表(Live 页与 Overlay 派生共用)。
pub fn active_deck_list(cx: &gpui::App) -> Option<DeckList> {
    cx.global::<Stores>()
        .decks
        .as_ref()?
        .active()
        .ok()
        .flatten()
        .map(|deck| deck.list)
}

#[derive(Default)]
pub struct Stores {
    pub decks: Option<Rc<dyn DeckRepository>>,
    pub history: Option<Rc<dyn GameHistoryRepository>>,
    /// 打开失败原因(Settings 页展示)。
    pub open_error: Option<String>,
}

impl Global for Stores {}

impl Stores {
    /// 打开应用数据目录下的库。失败只记原因,不 panic。
    pub fn open_default() -> Self {
        let path = hs_platform::default_card_db_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return Self {
                    open_error: Some(format!("无法创建数据目录 {}: {err}", parent.display())),
                    ..Self::default()
                };
            }
        }
        match SqliteDeckStore::open(&path) {
            Ok(store) => {
                let store = Rc::new(store);
                Self {
                    decks: Some(store.clone()),
                    history: Some(store),
                    open_error: None,
                }
            }
            Err(err) => Self {
                open_error: Some(format!("打开 {} 失败: {err}", path.display())),
                ..Self::default()
            },
        }
    }
}
