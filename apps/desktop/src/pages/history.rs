//! Game History 页(M2 Track D):对局列表 + 按牌组统计(§10.2)。
//!
//! 仓储经 `crate::stores::Stores` 注入;未就绪显示空态。

use gpui::{div, prelude::*, px, Context, Window};
use hs_decks::{GameFilter, GameHistoryRepository};
use hs_ui::components as c;
use hs_ui::tokens as t;
use std::rc::Rc;

use crate::stores::Stores;

pub struct HistoryView;

impl HistoryView {
    pub fn new() -> Self {
        Self
    }

    fn repo(cx: &gpui::App) -> Option<Rc<dyn GameHistoryRepository>> {
        cx.global::<Stores>().history.clone()
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let repo = Self::repo(cx);

        // GameFilter 是 #[non_exhaustive]：跨 crate 只能 default 后改字段。
        let mut filter = GameFilter::default();
        filter.limit = Some(50);
        let (games, stats) = match &repo {
            Some(repo) => (
                repo.list_games(&filter).unwrap_or_default(),
                repo.deck_stats().unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };

        let stats_panel =
            c::panel()
                .child(c::section_title("按牌组统计"))
                .children(if stats.is_empty() {
                    vec![c::empty_state("暂无统计")]
                } else {
                    stats
                        .iter()
                        .map(|s| {
                            let decided = s.wins + s.losses;
                            let rate = if decided > 0 {
                                format!("{:.0}%", s.wins as f32 * 100.0 / decided as f32)
                            } else {
                                "-".into()
                            };
                            c::list_row()
                                .child(div().flex_grow().truncate().child(s.deck_name.clone()))
                                .child(c::badge(format!("{}胜{}负", s.wins, s.losses)))
                                .child(c::badge(rate))
                        })
                        .collect()
                });

        let games_panel = div()
            .id("history-games")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap(t::space_1())
            .p(t::space_3())
            .rounded_md()
            .bg(t::bg_panel())
            .overflow_y_scroll()
            .child(c::section_title("最近对局"))
            .children(if repo.is_none() {
                vec![c::empty_state("存储未就绪(Track B 集成后可用)")]
            } else if games.is_empty() {
                vec![c::empty_state("还没有对局记录:打一局就有了")]
            } else {
                games
                    .iter()
                    .map(|g| {
                        let result = match (g.winner, g.friendly_player) {
                            (Some(w), Some(f)) if w == f => "胜",
                            (Some(_), Some(_)) => "负",
                            _ => "?",
                        };
                        c::list_row()
                            .child(c::badge(result))
                            .child(div().flex_grow().truncate().child(format!(
                                "{} vs {} · {}",
                                g.friendly_class.clone().unwrap_or_else(|| "?".into()),
                                g.opposing_class.clone().unwrap_or_else(|| "?".into()),
                                g.started_at,
                            )))
                            .children(g.raw_log_path.is_some().then(|| c::badge("日志已存档")))
                    })
                    .collect()
            });

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .h_full()
            .p(t::space_4())
            .gap(t::space_3())
            .bg(t::bg_content())
            .text_color(t::text_primary())
            .child(c::page_title("Game History"))
            .child(stats_panel)
            .child(games_panel)
    }
}
