//! Decks 页(M2 Track D):牌组列表/激活/归档 + 剪贴板导入 Deckstring。
//!
//! 自动导入走 Decks.log(Track B 接线);本页手动导入为兜底与分享场景。
//! 仓储经 `crate::stores::Stores` 注入;未就绪时显示空态。

use gpui::{div, prelude::*, px, ClipboardItem, Context, Window};
use hs_decks::{Deck, DeckDraft, DeckRepository};
use hs_types::DeckId;
use hs_ui::components as c;
use hs_ui::tokens as t;
use std::rc::Rc;

use crate::stores::Stores;

pub struct DecksView {
    /// 最近一次操作提示(导入结果/错误)。
    status: Option<String>,
}

impl DecksView {
    pub fn new() -> Self {
        Self { status: None }
    }

    fn repo(cx: &gpui::App) -> Option<Rc<dyn DeckRepository>> {
        cx.global::<Stores>().decks.clone()
    }

    /// 从剪贴板导入 deckstring(取第一行以 AAE 开头的文本)。
    fn import_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = Self::repo(cx) else {
            self.status = Some("存储未就绪".into());
            return;
        };
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        let Some(line) = text.lines().map(str::trim).find(|l| l.starts_with("AAE")) else {
            self.status = Some("剪贴板里没有 Deckstring(以 AAE 开头的行)".into());
            return;
        };
        // 名称:剪贴板常见「### 名字」格式,取一下;没有就用占位。
        let name = text
            .lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix("### "))
            .unwrap_or("导入的牌组")
            .to_string();
        match hs_deckstrings::decode(line) {
            Ok(list) => {
                let draft = DeckDraft::new(name, class_of(&list), list);
                self.status = Some(match repo.create(&draft) {
                    Ok(deck) => format!("已导入「{}」", deck.name),
                    Err(e) => format!("导入失败: {e}"),
                });
            }
            Err(e) => self.status = Some(format!("Deckstring 解析失败: {e}")),
        }
    }
}

/// 从英雄 dbf 推职业需要查卡牌库;接入前先占位。
fn class_of(_list: &hs_deckstrings::DeckList) -> String {
    "UNKNOWN".to_string()
}

fn deck_row(deck: &Deck, active: bool, cx: &mut Context<DecksView>) -> gpui::Div {
    let id = deck.id;
    let archived = deck.archived;
    let card_count: u32 = deck.list.cards.iter().map(|(_, n)| *n as u32).sum();
    div()
        .flex()
        .items_center()
        .gap(t::space_2())
        .px(t::space_3())
        .py(t::space_2())
        .rounded_md()
        .bg(if active {
            t::bg_active()
        } else {
            t::bg_panel()
        })
        .child(
            div()
                .flex_grow()
                .truncate()
                .text_size(t::font_sm())
                .text_color(if active {
                    t::accent()
                } else {
                    t::text_primary()
                })
                .child(format!("{} · {} · {}张", deck.name, deck.class, card_count)),
        )
        .children(active.then(|| c::badge("活动")))
        .children(archived.then(|| c::badge("已归档")))
        .child(
            div()
                .id(gpui::SharedString::from(format!("deck-activate-{}", id.0)))
                .px(t::space_2())
                .rounded_sm()
                .cursor_pointer()
                .text_size(t::font_xs())
                .text_color(t::text_muted())
                .hover(|el| el.bg(t::bg_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(repo) = DecksView::repo(cx) {
                        let target = if active { None } else { Some(id) };
                        this.status = repo.set_active(target).err().map(|e| e.to_string());
                    }
                    cx.notify();
                }))
                .child(if active { "取消激活" } else { "激活" }),
        )
        .child(
            div()
                .id(gpui::SharedString::from(format!("deck-archive-{}", id.0)))
                .px(t::space_2())
                .rounded_sm()
                .cursor_pointer()
                .text_size(t::font_xs())
                .text_color(t::text_muted())
                .hover(|el| el.bg(t::bg_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(repo) = DecksView::repo(cx) {
                        this.status = repo
                            .set_archived(id, !archived)
                            .err()
                            .map(|e| e.to_string());
                    }
                    cx.notify();
                }))
                .child(if archived { "取消归档" } else { "归档" }),
        )
        .child(
            div()
                .id(gpui::SharedString::from(format!("deck-copy-{}", id.0)))
                .px(t::space_2())
                .rounded_sm()
                .cursor_pointer()
                .text_size(t::font_xs())
                .text_color(t::text_muted())
                .hover(|el| el.bg(t::bg_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(repo) = DecksView::repo(cx) {
                        this.status = match repo.get(id).map(|d| hs_deckstrings::encode(&d.list)) {
                            Ok(Ok(s)) => {
                                cx.write_to_clipboard(ClipboardItem::new_string(s));
                                Some("Deckstring 已复制到剪贴板".into())
                            }
                            Ok(Err(e)) => Some(format!("编码失败: {e}")),
                            Err(e) => Some(format!("读取失败: {e}")),
                        };
                    }
                    cx.notify();
                }))
                .child("复制码"),
        )
}

impl Render for DecksView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = Self::repo(cx);
        let (decks, active_id): (Vec<Deck>, Option<DeckId>) = match &repo {
            Some(repo) => (
                repo.list(true).unwrap_or_default(),
                repo.active().ok().flatten().map(|d| d.id),
            ),
            None => (Vec::new(), None),
        };

        let toolbar = div()
            .flex()
            .gap(t::space_2())
            .items_center()
            .child(
                c::button("decks-import-clipboard", "从剪贴板导入 Deckstring").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.import_from_clipboard(cx);
                        cx.notify();
                    }),
                ),
            )
            .children(self.status.clone().map(|s| {
                div()
                    .text_size(t::font_sm())
                    .text_color(t::text_faint())
                    .child(s)
            }));

        let body = if repo.is_none() {
            c::empty_state("存储未就绪(Track B 集成后可用)").into_any_element()
        } else if decks.is_empty() {
            c::empty_state("还没有牌组:在游戏里选牌组排队会自动导入,或从剪贴板导入")
                .into_any_element()
        } else {
            div()
                .id("decks-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .gap(t::space_2())
                .overflow_y_scroll()
                .children(
                    decks
                        .iter()
                        .map(|d| deck_row(d, active_id == Some(d.id), cx)),
                )
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .h_full()
            .p(t::space_4())
            .gap(t::space_3())
            .bg(t::bg_content())
            .text_color(t::text_primary())
            .child(c::page_title("Decks"))
            .child(toolbar)
            .child(body)
    }
}
