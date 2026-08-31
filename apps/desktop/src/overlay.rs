//! Overlay 窗口与控制器（M0 Track A，TA-02/04/05）。
//!
//! GPUI 负责创建窗口与绘制内容；创建后立即通过 raw-window-handle 取得
//! NSWindow，交给 `hs-overlay-platform` 追加 AppKit 行为（穿透/层级/拖动）。

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    div, point, prelude::*, px, rgba, size, App, Bounds, Context, Global, Task, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use hs_overlay_platform::macos::MacOverlayPlatform;
use hs_overlay_platform::{OverlayMode, OverlayPlatform};
use hs_types::LogicalRect;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Overlay 跟随目标（TA-05）。
#[derive(Clone, Debug, PartialEq)]
pub enum FollowTarget {
    None,
    /// 模拟矩形：绕基准位置缓慢画圆，验证 setFrame 跟随链路。
    Simulated,
    /// 跟随指定进程名的第一个可见窗口（CGWindowList）。
    Window {
        owner: String,
    },
}

/// 全局 Overlay 控制器。主窗口 UI 与跟随任务都通过它操作 Overlay。
pub struct OverlayController {
    pub platform: Rc<MacOverlayPlatform>,
    pub overlay: Option<WindowHandle<OverlayView>>,
    pub mode: OverlayMode,
    pub topmost: bool,
    pub follow: FollowTarget,
    pub last_status: String,
    follow_task: Option<Task<()>>,
    sim_phase: f32,
}

impl Global for OverlayController {}

impl OverlayController {
    pub fn new() -> Self {
        Self {
            platform: Rc::new(MacOverlayPlatform::new()),
            overlay: None,
            mode: OverlayMode::Hidden,
            topmost: true,
            follow: FollowTarget::None,
            last_status: String::from("overlay not created"),
            follow_task: None,
            sim_phase: 0.0,
        }
    }
}

/// Overlay 位置持久化（TA-04）。全局 point 坐标、顶左原点。
fn frame_store_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support/HearthAdvisor/overlay-frame.json"),
    )
}

fn load_saved_frame() -> Option<LogicalRect> {
    let raw = std::fs::read_to_string(frame_store_path()?).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_frame(rect: LogicalRect) {
    let Some(path) = frame_store_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&rect) {
        let _ = std::fs::write(path, json);
    }
}

const DEFAULT_FRAME: LogicalRect = LogicalRect {
    x: 240.0,
    y: 180.0,
    width: 340.0,
    height: 200.0,
};

/// 创建 Overlay 窗口（幂等）。
pub fn ensure_overlay(cx: &mut App) {
    if cx.global::<OverlayController>().overlay.is_some() {
        return;
    }
    let frame = load_saved_frame().unwrap_or(DEFAULT_FRAME);
    let bounds = Bounds {
        origin: point(px(frame.x), px(frame.y)),
        size: size(px(frame.width), px(frame.height)),
    };
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            // 窗口刚创建即取原生句柄，存入控制器供 AppKit 配置使用
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::AppKit(appkit) = handle.as_raw() {
                    let native =
                        unsafe { MacOverlayPlatform::handle_from_ns_view(appkit.ns_view.as_ptr()) };
                    if let Some(native) = native {
                        let ctl = cx.global_mut::<OverlayController>();
                        match ctl.platform.configure(native) {
                            Ok(()) => ctl.last_status = "overlay configured (AppKit)".into(),
                            Err(e) => ctl.last_status = format!("configure failed: {e}"),
                        }
                    } else {
                        cx.global_mut::<OverlayController>().last_status =
                            "no NSWindow behind ns_view".into();
                    }
                } else {
                    cx.global_mut::<OverlayController>().last_status =
                        "non-AppKit raw window handle".into();
                }
            }
            cx.new(OverlayView::new)
        },
    );
    match opened {
        Ok(handle) => {
            let ctl = cx.global_mut::<OverlayController>();
            ctl.overlay = Some(handle);
        }
        Err(e) => {
            cx.global_mut::<OverlayController>().last_status = format!("open_window failed: {e}");
            return;
        }
    }
    set_mode(cx, OverlayMode::Passive);
}

/// 切换 Overlay 模式（TA-04）。离开 Interactive 时保存位置。
pub fn set_mode(cx: &mut App, mode: OverlayMode) {
    ensure_overlay(cx);
    let ctl = cx.global_mut::<OverlayController>();
    let leaving_interactive = ctl.mode == OverlayMode::Interactive && mode != ctl.mode;
    if leaving_interactive {
        if let Ok(frame) = ctl.platform.current_frame_points() {
            save_frame(frame);
        }
    }
    ctl.mode = mode;
    ctl.last_status = match ctl.platform.set_mode(mode) {
        Ok(()) => format!("mode = {mode:?}"),
        Err(e) => format!("set_mode failed: {e}"),
    };
    refresh_overlay(cx);
}

pub fn set_topmost(cx: &mut App, enabled: bool) {
    let ctl = cx.global_mut::<OverlayController>();
    ctl.topmost = enabled;
    ctl.last_status = match ctl.platform.set_topmost(enabled) {
        Ok(()) => format!("topmost = {enabled}"),
        Err(e) => format!("set_topmost failed: {e}"),
    };
    refresh_overlay(cx);
}

/// 设置跟随目标并启动/停止跟随任务（TA-05）。
pub fn set_follow(cx: &mut App, target: FollowTarget) {
    ensure_overlay(cx);
    {
        let ctl = cx.global_mut::<OverlayController>();
        ctl.follow = target.clone();
        ctl.follow_task = None; // 先停掉旧任务
        ctl.last_status = format!("follow = {target:?}");
    }
    if target == FollowTarget::None {
        return;
    }
    let task = cx.spawn(async move |cx| loop {
        let timer = match cx.update(|cx| cx.background_executor().timer(Duration::from_millis(120)))
        {
            Ok(t) => t,
            Err(_) => break,
        };
        timer.await;
        let alive = cx.update(follow_tick).unwrap_or(false);
        if !alive {
            break;
        }
    });
    cx.global_mut::<OverlayController>().follow_task = Some(task);
}

/// 单次跟随 tick：计算目标 frame 并 setFrame。返回 false 表示应停止。
fn follow_tick(cx: &mut App) -> bool {
    let (platform, target, phase) = {
        let ctl = cx.global_mut::<OverlayController>();
        ctl.sim_phase += 0.12;
        (ctl.platform.clone(), ctl.follow.clone(), ctl.sim_phase)
    };
    let frame = match &target {
        FollowTarget::None => return false,
        FollowTarget::Simulated => {
            let (dx, dy) = (phase.cos() * 60.0, phase.sin() * 60.0);
            Some(LogicalRect {
                x: 400.0 + dx,
                y: 300.0 + dy,
                width: 340.0,
                height: 200.0,
            })
        }
        FollowTarget::Window { owner } => {
            match hs_platform::macos_window::find_windows_by_owner(owner) {
                Ok(wins) => wins.first().map(|w| w.bounds_points),
                Err(_) => None,
            }
        }
    };
    if let Some(frame) = frame {
        let _ = platform.set_frame_points(frame);
    }
    refresh_overlay(cx);
    true
}

/// 让 Overlay 窗口重绘（模式边框、frame 信息展示）。
fn refresh_overlay(cx: &mut App) {
    let Some(handle) = cx.global::<OverlayController>().overlay else {
        return;
    };
    let _ = handle.update(cx, |_, _, cx| cx.notify());
}

/// Overlay 内容视图(M2 Track D):两块 Tracker 面板——我方牌库 + 对手已出牌。
/// Interactive(编辑)模式显示边框与拖动提示,Passive 全穿透。
pub struct OverlayView {
    session: Option<gpui::Entity<crate::session::GameSession>>,
    advice: Option<gpui::Entity<crate::advisor_session::AdviceSession>>,
    _session_observe: Option<gpui::Subscription>,
    _advice_observe: Option<gpui::Subscription>,
}

impl OverlayView {
    fn new(cx: &mut Context<Self>) -> Self {
        let session = cx
            .try_global::<crate::session::SessionHandle>()
            .map(|h| h.0.clone());
        let advice = cx
            .try_global::<crate::advisor_session::AdviceSessionHandle>()
            .map(|handle| handle.0.clone());
        let session_observe = session
            .as_ref()
            .map(|s| cx.observe(s, |_, _, cx| cx.notify()));
        let advice_observe = advice
            .as_ref()
            .map(|session| cx.observe(session, |_, _, cx| cx.notify()));
        Self {
            session,
            advice,
            _session_observe: session_observe,
            _advice_observe: advice_observe,
        }
    }
}

fn overlay_panel(title: &'static str) -> gpui::Div {
    use hs_ui::tokens as t;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .gap(t::space_1())
        .p(t::space_2())
        .rounded_md()
        .bg(t::overlay_panel_bg())
        .child(
            div()
                .text_size(t::font_xs())
                .text_color(t::accent())
                .child(title),
        )
}

fn overlay_row(text: String, badge: Option<String>) -> gpui::Div {
    use hs_ui::tokens as t;
    div()
        .flex()
        .items_center()
        .gap(t::space_1())
        .px(t::space_1())
        .rounded_sm()
        .bg(t::overlay_row_bg())
        .text_size(t::font_xs())
        .text_color(t::text_primary())
        .child(div().flex_grow().truncate().child(text))
        .children(badge.map(|b| {
            div()
                .text_size(t::font_xs())
                .text_color(t::accent())
                .child(b)
        }))
}

fn overlay_advice(model: Option<&crate::advice_presenter::OverlayAdviceModel>) -> gpui::Div {
    use hs_ui::tokens as t;

    let (headline, confidence, risk, color) = match model {
        Some(model) => (
            model.action.clone().unwrap_or_else(|| model.status.clone()),
            model.confidence.clone(),
            model.risk.clone(),
            match model.state {
                crate::advice_presenter::AdviceDisplayState::Ready => t::ok(),
                crate::advice_presenter::AdviceDisplayState::Updating => t::accent(),
                crate::advice_presenter::AdviceDisplayState::Unavailable => t::text_muted(),
                crate::advice_presenter::AdviceDisplayState::Failed => t::err(),
            },
        ),
        None => ("等待建议会话".into(), None, None, t::text_muted()),
    };

    div()
        .flex()
        .flex_col()
        .gap(t::space_1())
        .px(t::space_2())
        .py(t::space_1())
        .rounded_sm()
        .bg(t::overlay_row_bg())
        .child(
            div()
                .flex()
                .items_center()
                .gap(t::space_1())
                .text_size(t::font_xs())
                .child(div().text_color(color).child("建议"))
                .child(
                    div()
                        .flex_grow()
                        .truncate()
                        .text_color(t::text_primary())
                        .child(headline),
                )
                .children(
                    confidence.map(|value| div().text_color(t::accent()).truncate().child(value)),
                ),
        )
        .children(risk.map(|value| {
            div()
                .truncate()
                .text_size(t::font_xs())
                .text_color(t::warn())
                .child(format!("风险 · {value}"))
        }))
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use hs_ui::tokens as t;
        let ctl = cx.global::<OverlayController>();
        let mode = ctl.mode;

        let border_color = match mode {
            OverlayMode::Interactive => rgba(0xffd166ff),
            _ => rgba(0x00000000),
        };

        let active_deck = crate::stores::active_deck_list(cx);
        let state = self
            .session
            .as_ref()
            .map(|session| session.read(cx).state.clone());
        let view = state
            .as_ref()
            .map(|state| hs_tracker::derive(state, active_deck.as_ref()))
            .unwrap_or_default();
        let advice = self
            .advice
            .as_ref()
            .zip(state.as_ref())
            .map(|(advice, state)| {
                let advice = advice.read(cx);
                crate::advice_presenter::OverlayAdviceModel::from_current(
                    advice.status(),
                    state,
                    advice.current_for(state),
                )
            });

        let my_deck = overlay_panel("我方牌库").children(if view.my_deck.is_empty() {
            vec![overlay_row("等待对局…".into(), None)]
        } else {
            view.my_deck
                .iter()
                .map(|row| {
                    let name = crate::cards::row_label(cx, row.card_id.as_ref(), row.dbf_id);
                    overlay_row(name, Some(format!("x{}", row.count_remaining)))
                })
                .collect()
        });

        let opp_played = overlay_panel("对手已出牌").children(if view.opponent_played.is_empty() {
            vec![overlay_row("暂无".into(), None)]
        } else {
            view.opponent_played
                .iter()
                .rev()
                .take(20)
                .map(|row| {
                    let name = match row.card_id.as_ref() {
                        Some(card) => crate::cards::row_label(cx, Some(card), None),
                        None => format!("#{}", row.entity_id.0),
                    };
                    overlay_row(name, Some(format!("T{}", row.turn)))
                })
                .collect()
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .p_1()
            .gap_1()
            .rounded_lg()
            .border_2()
            .border_color(border_color)
            .text_color(t::text_primary())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(t::font_xs())
                    .text_color(t::text_faint())
                    .child(format!(
                        "对手手牌: {} · 回合 {}",
                        view.opponent_hand.len(),
                        view.game.turn
                    ))
                    .children(matches!(mode, OverlayMode::Interactive).then(|| {
                        div()
                            .text_color(t::accent())
                            .child("编辑模式:拖动调整,切回 Passive 保存")
                    })),
            )
            .child(overlay_advice(advice.as_ref()))
            .child(my_deck)
            .child(opp_played)
    }
}
