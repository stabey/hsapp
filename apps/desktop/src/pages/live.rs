//! Live Game 页(M2 Track D):TrackerView 全量展示(设计文档 §15.2 M2 子集)。
//!
//! 数据源:观察共享 GameSession → `hs_tracker::derive`;本页不复制状态。

use gpui::{div, prelude::*, px, Context, Entity, Subscription, Window};
use hs_tracker::{derive, TrackerView};
use hs_ui::components as c;
use hs_ui::tokens as t;

use crate::advice_presenter::{AdviceDisplayState, LiveAdviceLineModel, LiveAdviceModel};
use crate::advisor_session::AdviceSession;
use crate::session::GameSession;
use crate::stores::Stores;

pub struct LiveView {
    session: Entity<GameSession>,
    advice: Entity<AdviceSession>,
    _session_observe: Subscription,
    _advice_observe: Subscription,
}

impl LiveView {
    pub fn new(
        session: Entity<GameSession>,
        advice: Entity<AdviceSession>,
        cx: &mut Context<Self>,
    ) -> Self {
        let session_observe = cx.observe(&session, |_, _, cx| cx.notify());
        let advice_observe = cx.observe(&advice, |_, _, cx| cx.notify());
        Self {
            session,
            advice,
            _session_observe: session_observe,
            _advice_observe: advice_observe,
        }
    }
}

fn side_counters(title: &'static str, s: &hs_tracker::SideCounters) -> gpui::Div {
    c::panel()
        .child(c::section_title(title))
        .child(c::kv("武器", s.weapons.to_string()))
        .child(c::kv("地点", s.locations.to_string()))
        .child(c::kv("任务", s.quests.to_string()))
        .child(c::kv("秘密", s.secrets.to_string()))
        .child(c::kv("疲劳", s.fatigue.to_string()))
}

fn scroll_panel(id: &'static str, title: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .gap(t::space_1())
        .p(t::space_3())
        .rounded_md()
        .bg(t::bg_panel())
        .overflow_y_scroll()
        .child(c::section_title(title))
}

fn advice_state_color(state: AdviceDisplayState) -> gpui::Rgba {
    match state {
        AdviceDisplayState::Ready => t::ok(),
        AdviceDisplayState::Updating => t::accent(),
        AdviceDisplayState::Unavailable => t::text_muted(),
        AdviceDisplayState::Failed => t::err(),
    }
}

fn advice_line(label: String, line: &LiveAdviceLineModel) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(t::space_1())
        .p(t::space_2())
        .rounded_md()
        .bg(t::bg_active())
        .child(
            div()
                .flex()
                .items_center()
                .gap(t::space_2())
                .child(c::badge(label))
                .child(
                    div()
                        .text_size(t::font_xs())
                        .text_color(t::warn())
                        .child(line.lethal.clone()),
                ),
        )
        .child(
            div()
                .text_size(t::font_sm())
                .text_color(t::text_primary())
                .child(line.action.clone()),
        )
        .children(line.win_point.as_ref().map(|win_point| {
            div()
                .text_size(t::font_xs())
                .text_color(t::ok())
                .child(format!("赢点 · {win_point}"))
        }))
        .child(
            div()
                .text_size(t::font_xs())
                .text_color(t::text_muted())
                .child(line.explanation.clone()),
        )
        .child(
            div()
                .text_size(t::font_xs())
                .text_color(t::text_muted())
                .child(format!(
                    "{} · {}",
                    line.score_total,
                    line.score_components.join(" · ")
                )),
        )
        .child(
            div()
                .text_size(t::font_xs())
                .text_color(t::accent())
                .child(line.confidence.clone()),
        )
        .children(line.previews.iter().map(|preview| {
            div()
                .text_size(t::font_xs())
                .text_color(t::text_muted())
                .child(preview.clone())
        }))
        .children(line.risks.iter().map(|risk| {
            div()
                .text_size(t::font_xs())
                .text_color(t::warn())
                .child(format!("风险 · {risk}"))
        }))
}

fn advice_panel(model: &LiveAdviceModel) -> gpui::Stateful<gpui::Div> {
    c::panel()
        .id("live-advice")
        .flex_none()
        .max_h(px(280.))
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .items_center()
                .gap(t::space_2())
                .child(c::section_title("当前建议"))
                .child(
                    div()
                        .text_size(t::font_sm())
                        .text_color(advice_state_color(model.state))
                        .child(model.status.clone()),
                )
                .child(
                    div()
                        .text_size(t::font_xs())
                        .text_color(t::text_faint())
                        .child(format!("{} · {}", model.build, model.revision)),
                ),
        )
        .children(
            model
                .confidence
                .as_ref()
                .map(|value| c::kv("置信度", value.clone())),
        )
        .children(
            model
                .coverage
                .as_ref()
                .map(|value| c::kv("规则覆盖", value.clone())),
        )
        .children(
            model
                .search_completeness
                .as_ref()
                .map(|value| c::kv("搜索证据", value.clone())),
        )
        .children(
            model
                .primary
                .as_ref()
                .map(|line| advice_line("首选".into(), line)),
        )
        .children(
            model
                .alternatives
                .iter()
                .enumerate()
                .map(|(index, line)| advice_line(format!("备选 {}", index + 1), line)),
        )
        .children(model.warnings.iter().map(|warning| {
            div()
                .text_size(t::font_xs())
                .text_color(t::warn())
                .child(format!("提示 · {warning}"))
        }))
}

impl Render for LiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_deck = crate::stores::active_deck_list(cx);
        let deck_label = match cx.global::<Stores>().decks.is_some() {
            true => active_deck
                .as_ref()
                .map(|_| "已激活".to_string())
                .unwrap_or_else(|| "未激活(进游戏选牌组会自动识别)".into()),
            false => "存储未就绪".into(),
        };

        let (source, record_status, state) = {
            let session = self.session.read(cx);
            (
                session.source.clone(),
                session
                    .last_record_status
                    .clone()
                    .unwrap_or_else(|| "尚未落库".into()),
                session.state.clone(),
            )
        };
        let view = derive(&state, active_deck.as_ref()) as TrackerView;
        let advice = {
            let session = self.advice.read(cx);
            LiveAdviceModel::from_current(session.status(), &state, session.current_for(&state))
        };

        let meta = c::panel()
            .child(c::section_title("对局"))
            .child(c::kv("日志", source.to_string()))
            .child(c::kv("落库", record_status))
            .child(c::kv("牌组", deck_label))
            .child(c::kv(
                "GameId",
                view.game
                    .game_id
                    .map(|g| g.0.to_string())
                    .unwrap_or_else(|| "-".into()),
            ))
            .child(c::kv("阶段", format!("{:?}", view.game.phase)))
            .child(c::kv("回合", view.game.turn.to_string()))
            .child(c::kv(
                "友方",
                view.game
                    .friendly_player
                    .map(|p| format!("P{}", p.0))
                    .unwrap_or_else(|| "-".into()),
            ))
            .child(c::kv(
                "胜者",
                view.game
                    .winner
                    .map(|p| format!("P{}", p.0))
                    .unwrap_or_else(|| "-".into()),
            ))
            .child(c::kv("Revision", view.game.revision.to_string()));

        let my_deck =
            scroll_panel("live-my-deck", "我方牌库").children(if view.my_deck.is_empty() {
                vec![c::empty_state("暂无牌库信息(等待对局/Track C 派生)")]
            } else {
                view.my_deck
                    .iter()
                    .map(|row| {
                        let name = crate::cards::row_label(cx, row.card_id.as_ref(), row.dbf_id);
                        c::list_row()
                            .child(div().flex_grow().truncate().child(name))
                            .child(c::badge(format!(
                                "{}/{}",
                                row.count_remaining,
                                row.count_initial.max(row.count_remaining)
                            )))
                    })
                    .collect()
            });

        let opp_played = scroll_panel("live-opp-played", "对手已出牌").children(
            if view.opponent_played.is_empty() {
                vec![c::empty_state("对手还没出过牌")]
            } else {
                view.opponent_played
                    .iter()
                    .map(|row| {
                        let name = match row.card_id.as_ref() {
                            Some(card) => crate::cards::row_label(cx, Some(card), None),
                            None => format!("#{}", row.entity_id.0),
                        };
                        c::list_row()
                            .child(c::badge(format!("T{}", row.turn)))
                            .child(div().flex_grow().truncate().child(name))
                            .children(row.created.then(|| c::badge("生成")))
                    })
                    .collect()
            },
        );

        let opp_hand = c::panel()
            .child(c::section_title(format!(
                "对手手牌({})",
                view.opponent_hand.len()
            )))
            .children(view.opponent_hand.iter().map(|h| {
                let held = view.game.turn.saturating_sub(h.since_turn);
                c::list_row()
                    .child(div().flex_grow().child(match &h.known_card {
                        Some(card) => crate::cards::row_label(cx, Some(card), None),
                        None => "未知".into(),
                    }))
                    .child(c::badge(format!("持有{held}回合")))
            }));

        let graveyards = div()
            .flex()
            .gap(t::space_3())
            .child(
                c::panel()
                    .flex_1()
                    .child(c::section_title(format!(
                        "我方墓地({})",
                        view.graveyards[0].len()
                    )))
                    .children(view.graveyards[0].iter().take(20).map(|e| {
                        c::list_row().child(match e.card_id.as_ref() {
                            Some(card) => crate::cards::row_label(cx, Some(card), None),
                            None => format!("#{}", e.entity_id.0),
                        })
                    })),
            )
            .child(
                c::panel()
                    .flex_1()
                    .child(c::section_title(format!(
                        "对方墓地({})",
                        view.graveyards[1].len()
                    )))
                    .children(view.graveyards[1].iter().take(20).map(|e| {
                        c::list_row().child(match e.card_id.as_ref() {
                            Some(card) => crate::cards::row_label(cx, Some(card), None),
                            None => format!("#{}", e.entity_id.0),
                        })
                    })),
            );

        let counters = div()
            .flex()
            .gap(t::space_3())
            .child(side_counters("我方计数", &view.counters.friendly))
            .child(side_counters("对方计数", &view.counters.opposing));

        div()
            .id("live-page")
            .flex()
            .flex_col()
            .flex_grow()
            .h_full()
            .overflow_y_scroll()
            .p(t::space_4())
            .gap(t::space_3())
            .bg(t::bg_content())
            .text_color(t::text_primary())
            .child(c::page_title("Live Game"))
            .child(advice_panel(&advice))
            .child(div().flex().gap(t::space_3()).child(meta))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(180.))
                    .gap(t::space_3())
                    .child(my_deck)
                    .child(opp_played),
            )
            .child(opp_hand)
            .child(graveyards)
            .child(counters)
    }
}
