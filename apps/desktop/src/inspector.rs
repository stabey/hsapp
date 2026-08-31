//! 开发者检查器(M0 Track D 实装,M2 改为观察共享 GameSession)。
//!
//! 管道归 `crate::session::GameSession` 所有;本视图只保留 UI 态
//! (选中、步进游标、步进缓存),数据一律读会话实体。

use gpui::{div, prelude::*, px, rgb, rgba, Context, Entity, SharedString, Subscription, Window};
use hs_observed_state::ObservedGameState;
use hs_types::EntityId;

use crate::session::{GameSession, FIXTURE_LOG};

/// 每栏最多画这么多行。整晚 Power.log 有几万行,全量克隆+布局会卡成彩虹圈。
const INSPECTOR_ROW_CAP: usize = 2_000;

pub struct InspectorView {
    session: Entity<GameSession>,
    /// 步进游标:Some(i) = 展示前 i 个事件归约的状态;None = 实时模式。
    step_cursor: Option<usize>,
    /// 步进状态缓存 (cursor, state),避免每帧重算。
    stepped_state: Option<(usize, ObservedGameState)>,
    selected_packet: Option<usize>,
    _observe: Subscription,
}

impl InspectorView {
    pub fn new(session: Entity<GameSession>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&session, |_, _, cx| cx.notify());
        Self {
            session,
            step_cursor: None,
            stepped_state: None,
            selected_packet: None,
            _observe: observe,
        }
    }

    /// 设置步进游标并重算缓存。后退 = 从头 reduce 到 cursor(O(n),
    /// 一局事件量毫秒级,M1 不做快照)。
    fn set_cursor(&mut self, cursor: Option<usize>, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        self.step_cursor = cursor.map(|c| c.min(session.events.len()));
        self.stepped_state = self.step_cursor.map(|c| (c, session.replay_to(c)));
    }
}

/// 步进工具条小按钮。
fn step_button(
    id: &'static str,
    label: &'static str,
    cx: &mut Context<InspectorView>,
    on_click: impl Fn(&mut InspectorView, &mut Context<InspectorView>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(0x2b3446))
        .cursor_pointer()
        .text_sm()
        .hover(|el| el.bg(rgb(0x38445c)))
        .on_click(cx.listener(move |this, _, _, cx| {
            on_click(this, cx);
            cx.notify();
        }))
        .child(label)
}

fn column(title: impl Into<SharedString>, id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .flex_1()
        // 允许列在窄窗口下收缩到内容宽度以下（否则长文本把右侧列挤出视口）
        .min_w(px(0.))
        .h_full()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(rgb(0x181d27))
        .overflow_y_scroll()
        .child(
            div()
                .text_color(rgb(0xffd166))
                .text_sm()
                .child(title.into()),
        )
}

impl Render for InspectorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_packet;

        // 先把渲染需要的数据从会话读出来(SharedString/small copy 均廉价),
        // 避免 session.read 借用与 cx.listener 可变借用冲突。
        let (
            source,
            raw_rows,
            packet_window,
            event_window,
            raw_total,
            packet_total,
            event_total,
            live_state,
            selected_offsets,
        ) = {
            let session = self.session.read(cx);
            let raw_start = session.raw_rows.len().saturating_sub(INSPECTOR_ROW_CAP);
            let packet_start = session.packet_rows.len().saturating_sub(INSPECTOR_ROW_CAP);
            let event_start = session.event_rows.len().saturating_sub(INSPECTOR_ROW_CAP);
            let raw_rows: Vec<(u64, SharedString)> = session.raw_rows[raw_start..]
                .iter()
                .map(|r| (r.offset, r.text.clone()))
                .collect();
            let packet_window: Vec<(usize, SharedString)> = session.packet_rows[packet_start..]
                .iter()
                .enumerate()
                .map(|(i, p)| (packet_start + i, p.label.clone()))
                .collect();
            let event_window: Vec<(SharedString, usize, Option<EntityId>)> = session.event_rows
                [event_start..]
                .iter()
                .map(|e| (e.label.clone(), e.packet_idx, e.entity))
                .collect();
            let selected_offsets: Vec<u64> = selected
                .and_then(|i| session.packet_rows.get(i))
                .map(|p| p.record_offsets.clone())
                .unwrap_or_default();
            (
                session.source.clone(),
                raw_rows,
                packet_window,
                event_window,
                session.raw_rows.len(),
                session.packet_rows.len(),
                session.events.len(),
                session.state.clone(),
                selected_offsets,
            )
        };

        let toolbar = div()
            .flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .id("load-fixture")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x2b3446))
                    .cursor_pointer()
                    .text_sm()
                    .hover(|el| el.bg(rgb(0x38445c)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.session.update(cx, |session, cx| {
                            session.open_path(FIXTURE_LOG.to_string(), cx);
                        });
                    }))
                    .child("载入 fixture 日志"),
            )
            .child(
                div()
                    .id("detect-real")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x2b3446))
                    .cursor_pointer()
                    .text_sm()
                    .hover(|el| el.bg(rgb(0x38445c)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.session.update(cx, |session, cx| {
                            session.detect_real_log(cx);
                        });
                    }))
                    .child("检测真实日志"),
            )
            .child(div().text_sm().text_color(rgb(0x7f8a9d)).child(source));

        let cursor = self.step_cursor;
        let step_bar = div()
            .flex()
            .gap_1()
            .items_center()
            .child(step_button(
                "step-toggle",
                if cursor.is_some() {
                    "步进: 开"
                } else {
                    "步进: 关"
                },
                cx,
                move |this, cx| {
                    let next = if this.step_cursor.is_some() {
                        None
                    } else {
                        Some(this.session.read(cx).events.len())
                    };
                    this.set_cursor(next, cx);
                },
            ))
            .child(step_button("step-home", "|<", cx, |this, cx| {
                if this.step_cursor.is_some() {
                    this.set_cursor(Some(0), cx);
                }
            }))
            .child(step_button("step-back", "<", cx, |this, cx| {
                if let Some(c) = this.step_cursor {
                    this.set_cursor(Some(c.saturating_sub(1)), cx);
                }
            }))
            .child(step_button("step-fwd", ">", cx, |this, cx| {
                if let Some(c) = this.step_cursor {
                    this.set_cursor(Some(c + 1), cx);
                }
            }))
            .child(step_button("step-end", ">|", cx, |this, cx| {
                if this.step_cursor.is_some() {
                    let end = this.session.read(cx).events.len();
                    this.set_cursor(Some(end), cx);
                }
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x7f8a9d))
                    .child(match cursor {
                        Some(c) => format!("事件 {c}/{event_total}"),
                        None => format!("实时 ({event_total} 事件)"),
                    }),
            );

        let raw_title = if raw_total > INSPECTOR_ROW_CAP {
            format!("Raw 最近{INSPECTOR_ROW_CAP}/{raw_total}")
        } else {
            "Raw".into()
        };
        let packet_title = if packet_total > INSPECTOR_ROW_CAP {
            format!("Packet 最近{INSPECTOR_ROW_CAP}/{packet_total}")
        } else {
            "Packet".into()
        };
        let event_title = if event_total > INSPECTOR_ROW_CAP {
            format!("Event 最近{INSPECTOR_ROW_CAP}/{event_total}")
        } else {
            "Event".into()
        };

        let raw_col =
            column(raw_title, "col-raw").children(raw_rows.iter().map(|(offset, text)| {
                let hit = selected_offsets.contains(offset);
                div()
                    .text_sm()
                    .truncate()
                    .when(hit, |el| el.bg(rgb(0x2b3446)).text_color(rgb(0xffd166)))
                    .when(!hit, |el| el.text_color(rgb(0x9aa3b2)))
                    .child(text.clone())
            }));

        let packet_col = column(packet_title, "col-packet").children(packet_window.iter().map(
            |(idx, label)| {
                let idx = *idx;
                let active = selected == Some(idx);
                div()
                    .id(idx)
                    .text_sm()
                    .truncate()
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(active, |el| el.bg(rgb(0x2b3446)).text_color(rgb(0xffd166)))
                    .when(!active, |el| {
                        el.text_color(rgb(0xc3cad6))
                            .hover(|el| el.bg(rgb(0x232a38)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_packet = if this.selected_packet == Some(idx) {
                            None
                        } else {
                            Some(idx)
                        };
                        cx.notify();
                    }))
                    .child(label.clone())
            },
        ));

        let event_start = event_total.saturating_sub(event_window.len());
        let event_col =
            column(event_title, "col-event").children(event_window.iter().enumerate().map(
                |(i, (label, packet_idx, _))| {
                    let actual = event_start + i;
                    let dim = selected.is_some() && selected != Some(*packet_idx);
                    // 步进模式下：游标之后的事件淡出，游标处高亮。
                    let beyond = cursor.is_some_and(|c| actual >= c);
                    let at_cursor = cursor.is_some_and(|c| c > 0 && actual == c - 1);
                    div()
                        .text_sm()
                        .truncate()
                        .when(at_cursor, |el| {
                            el.bg(rgb(0x2b3446)).text_color(rgb(0xffd166))
                        })
                        .when(!at_cursor && (dim || beyond), |el| {
                            el.text_color(rgba(0x9aa3b240))
                        })
                        .when(!at_cursor && !dim && !beyond, |el| {
                            el.text_color(rgb(0xc3cad6))
                        })
                        .child(label.clone())
                },
            ));

        let selected_entity: Option<EntityId> = selected.and_then(|i| {
            event_window
                .iter()
                .filter(|(_, packet_idx, _)| *packet_idx == i)
                .find_map(|(_, _, entity)| *entity)
        });

        // 步进/实时模式下当前展示的状态。
        let state = match (self.step_cursor, &self.stepped_state) {
            (Some(cursor), Some((cached, state))) if *cached == cursor => state.clone(),
            _ => live_state,
        };

        let mut entity_ids: Vec<&EntityId> = state.entities.keys().collect();
        entity_ids.sort_by_key(|e| e.0);
        let entity_col = column("Entity", "col-entity")
            .child(
                div()
                    .text_sm()
                    .truncate()
                    .text_color(rgb(0x7f8a9d))
                    .child(format!(
                        "game={:?} phase={:?} turn={} rev={} warn={}",
                        state.game_id.map(|g| g.0),
                        state.phase,
                        state.turn,
                        state.revision,
                        state.warning_count
                    )),
            )
            .children(entity_ids.into_iter().map(|id| {
                let entity = &state.entities[id];
                let hit = selected_entity == Some(*id);
                div()
                    .text_sm()
                    .truncate()
                    .when(hit, |el| el.bg(rgb(0x2b3446)).text_color(rgb(0xffd166)))
                    .when(!hit, |el| el.text_color(rgb(0xc3cad6)))
                    .child(format!(
                        "#{} {} zone={:?} ctrl={:?} tags={}",
                        entity.entity_id.0,
                        entity.card_id.as_ref().map(|c| c.0.as_ref()).unwrap_or("-"),
                        entity.zone,
                        entity.controller.map(|p| p.0),
                        entity.tags.len()
                    ))
            }));

        // State 栏：Block 调用栈 / Options / Choices / Players（M1 R6）。
        let section = |label: &'static str| {
            div()
                .text_sm()
                .text_color(rgb(0xffd166))
                .pt_1()
                .child(label)
        };
        let line = |text: String| {
            div()
                .text_sm()
                .truncate()
                .text_color(rgb(0xc3cad6))
                .child(text)
        };
        let dim_line =
            |text: &'static str| div().text_sm().text_color(rgba(0x9aa3b280)).child(text);

        let mut state_col = column("State", "col-state");

        state_col = state_col.child(section("Players"));
        if state.players.is_empty() {
            state_col = state_col.child(dim_line("（无）"));
        }
        for (pid, player) in &state.players {
            let mut text = format!(
                "P{} entity=#{} playstate={:?}",
                pid, player.entity_id.0, player.playstate
            );
            if state.current_player.map(|p| p.0) == Some(*pid) {
                text.push_str(" (当前)");
            }
            if state.winner.map(|p| p.0) == Some(*pid) {
                text.push_str(" (胜)");
            }
            state_col = state_col.child(line(text));
        }

        state_col = state_col.child(section("Block 栈"));
        if state.block_stack.is_empty() {
            state_col = state_col.child(dim_line("（空）"));
        }
        for (depth, frame) in state.block_stack.iter().enumerate() {
            state_col = state_col.child(line(format!(
                "{}{} src={:?} tgt={:?}",
                "  ".repeat(depth),
                frame.block_type,
                frame.source.map(|e| e.0),
                frame.target.map(|e| e.0),
            )));
        }

        state_col = state_col.child(section("Options"));
        match &state.options {
            None => state_col = state_col.child(dim_line("（无）")),
            Some(options) => {
                state_col = state_col.child(line(format!(
                    "id={} 共 {} 项",
                    options.id,
                    options.options.len()
                )));
                for option in options.options.iter().take(12) {
                    state_col = state_col.child(line(format!(
                        "  [{}] {} entity={:?}{}",
                        option.index,
                        option.option_type,
                        option.entity.map(|e| e.0),
                        if option.error.is_some() { " (err)" } else { "" },
                    )));
                }
            }
        }

        state_col = state_col.child(section("Choices"));
        if state.choices.is_empty() {
            state_col = state_col.child(dim_line("（无）"));
        } else {
            state_col = state_col.child(line(format!("共 {} 份待处理选择", state.choices.len())));
            for (index, choices) in state.choices.iter().enumerate() {
                state_col = state_col.child(line(format!(
                    "  [{}/{}] id={} task={:?} rev={} player={:?} src={:?}",
                    index + 1,
                    state.choices.len(),
                    choices.id,
                    choices.task_list,
                    choices.issued_revision,
                    choices.player.map(|player| player.0),
                    choices.source.map(|entity| entity.0),
                )));
                state_col = state_col.child(line(format!(
                    "      {} {}~{} 选 {} 项",
                    choices.choice_type,
                    choices.min,
                    choices.max,
                    choices.entities.len(),
                )));
                for entity in choices.entities.iter().take(12) {
                    state_col = state_col.child(line(format!("      #{}", entity.0)));
                }
            }
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .gap_2()
            .p_2()
            .child(toolbar)
            .child(step_bar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .gap_2()
                    .min_h(px(0.))
                    .child(raw_col)
                    .child(packet_col)
                    .child(event_col)
                    .child(entity_col)
                    .child(state_col),
            )
    }
}

pub fn build(session: Entity<GameSession>, cx: &mut gpui::App) -> Entity<InspectorView> {
    cx.new(|cx| InspectorView::new(session, cx))
}
