//! 主窗口（M0 Track A，TA-01）：导航侧栏壳 + Overlay 控制区。
//! 各页面为占位符，Inspector 页由 Track D 实装。

use gpui::{div, prelude::*, rgb, rgba, App, ClickEvent, Context, Entity, Window};
use hs_diagnostics::ComponentHealth;
use hs_overlay_platform::OverlayMode;
use std::path::PathBuf;

use crate::inspector::InspectorView;
use crate::overlay::{self, FollowTarget, OverlayController};
use crate::pages::{decks::DecksView, history::HistoryView, live::LiveView, pools::PoolsView};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    Dashboard,
    Decks,
    Live,
    History,
    Inspector,
    Pools,
    Settings,
}

impl Page {
    const ALL: [Page; 7] = [
        Page::Dashboard,
        Page::Decks,
        Page::Live,
        Page::History,
        Page::Inspector,
        Page::Pools,
        Page::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::Dashboard => "Dashboard",
            Page::Decks => "Decks",
            Page::Live => "Live",
            Page::History => "History",
            Page::Inspector => "Inspector",
            Page::Pools => "Pools",
            Page::Settings => "Settings",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Page::Dashboard => "总览(M2 收尾时补)",
            Page::Decks => "牌组管理(M2)",
            Page::Live => "当前对局实时视图(M2)",
            Page::History => "对局历史与统计(M2)",
            Page::Inspector => "Raw / Packet / Event / Entity 四栏检查器（Track D）",
            Page::Pools => "Pool / Rule Inspector(M3)",
            Page::Settings => "日志路径、Overlay、语言设置（M1）",
        }
    }
}

pub struct MainView {
    page: Page,
    inspector: Entity<InspectorView>,
    live: Entity<LiveView>,
    decks: Entity<DecksView>,
    history: Entity<HistoryView>,
    pools: Entity<PoolsView>,
    /// CGWindowList 枚举出的候选进程名（去重），供跟随选择。
    window_owners: Vec<String>,
    /// log.config 一键写入的最近一次结果提示（Settings 页展示）。
    log_config_status: Option<String>,
}

impl MainView {
    pub fn new(
        inspector: Entity<InspectorView>,
        live: Entity<LiveView>,
        decks: Entity<DecksView>,
        history: Entity<HistoryView>,
        pools: Entity<PoolsView>,
    ) -> Self {
        Self {
            page: Page::Live,
            inspector,
            live,
            decks,
            history,
            pools,
            window_owners: Vec::new(),
            log_config_status: None,
        }
    }

    /// 当前生效的 log.config 路径（尊重 HS_LOG_CONFIG，同 MacPlatform）。
    fn log_config_path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("HS_LOG_CONFIG") {
            return Some(PathBuf::from(path));
        }
        let home = std::env::var_os("HOME")?;
        Some(hs_platform::default_macos_log_config_path(
            std::path::Path::new(&home),
        ))
    }

    fn write_log_config(&mut self) {
        let Some(path) = Self::log_config_path() else {
            self.log_config_status = Some("无法确定 log.config 路径（HOME 未设置）".into());
            return;
        };
        self.log_config_status = Some(match hs_platform::ensure_log_config(&path) {
            Ok(report) if !report.changed => "配置已满足要求，未做修改".into(),
            Ok(report) => {
                let backup = report
                    .backup_path
                    .map(|p| format!("，原文件已备份为 {}", p.display()))
                    .unwrap_or_default();
                format!("已写入{backup}。若炉石正在运行，需重启后生效")
            }
            Err(err) => format!("写入失败: {err}"),
        });
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> gpui::Div {
        let (path_text, health) = match Self::log_config_path() {
            Some(path) => {
                let health = hs_platform::validate_log_config(&path);
                (path.display().to_string(), health)
            }
            None => (
                "<HOME 未设置>".to_string(),
                ComponentHealth::Failed {
                    reason: "HOME is not set".into(),
                },
            ),
        };
        let (health_text, health_color) = match &health {
            ComponentHealth::Healthy => ("Healthy".to_string(), rgb(0x7bd88f)),
            ComponentHealth::Degraded { reason } => (format!("Degraded: {reason}"), rgb(0xffd166)),
            ComponentHealth::Failed { reason } => (format!("Failed: {reason}"), rgb(0xef6b73)),
        };

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .h_full()
            .p_6()
            .gap_3()
            .bg(rgb(0x1e2430))
            .text_color(rgb(0xe8ecf4))
            .child(div().text_lg().child("Settings"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x9aa3b2))
                    .child("日志配置 (log.config)"),
            )
            .child(div().text_sm().child(format!("路径: {path_text}")))
            .child(
                div()
                    .text_sm()
                    .text_color(health_color)
                    .child(format!("状态: {health_text}")),
            )
            .child(
                div()
                    .id("write-log-config")
                    .px_3()
                    .py_1()
                    .w_64()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(0x2b3446))
                    .text_color(rgb(0xffd166))
                    .hover(|el| el.bg(rgb(0x35425c)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.write_log_config();
                        cx.notify();
                    }))
                    .child("一键写入 log.config（写前自动备份）"),
            )
            .children(
                self.log_config_status
                    .clone()
                    .map(|status| div().text_sm().text_color(rgb(0x9aa3b2)).child(status)),
            )
            .child(
                div()
                    .pt_4()
                    .text_sm()
                    .text_color(rgb(0x9aa3b2))
                    .child("数据目录"),
            )
            .child(div().text_sm().child(format!(
                "应用数据目录: {}（HS_DATA_DIR 可覆盖）",
                hs_platform::app_data_dir().display()
            )))
            .child(
                div()
                    .pt_4()
                    .text_sm()
                    .text_color(rgb(0x9aa3b2))
                    .child("卡牌库"),
            )
            .child(div().text_sm().child({
                let names = cx.global::<crate::cards::CardNames>();
                match (names.is_ready(), &names.open_error) {
                    (true, _) => "已就绪（卡名显示为中文）".to_string(),
                    (false, Some(err)) => err.clone(),
                    (false, None) => {
                        "未导入卡牌：运行 cargo run -p hs-card-sync -- fetch --build latest --locale zhCN".to_string()
                    }
                }
            }))
            .child(
                div()
                    .pt_4()
                    .text_sm()
                    .text_color(rgb(0x9aa3b2))
                    .child("牌组自动导入 (Decks.log)"),
            )
            .child(div().text_sm().child(
                match cx.try_global::<crate::decks_watch::DecksWatchHandle>() {
                    Some(handle) => {
                        let watcher = handle.0.read(cx);
                        format!("{}（本次会话已导入 {}）", watcher.status, watcher.imported)
                    }
                    None => "未启动".to_string(),
                },
            ))
            .children(
                cx.global::<crate::stores::Stores>()
                    .open_error
                    .clone()
                    .map(|err| div().text_sm().text_color(rgb(0xef6b73)).child(err)),
            )
    }

    fn refresh_owners(&mut self) {
        self.window_owners.clear();
        if let Ok(windows) = hs_platform::macos_window::list_on_screen_windows() {
            for w in windows {
                if !w.owner_name.is_empty() && !self.window_owners.contains(&w.owner_name) {
                    self.window_owners.push(w.owner_name);
                }
            }
        }
    }
}

fn side_button(
    id: &'static str,
    label: impl Into<String>,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(active, |el| el.bg(rgb(0x2b3446)).text_color(rgb(0xffd166)))
        .when(!active, |el| {
            el.text_color(rgb(0xc3cad6))
                .hover(|el| el.bg(rgb(0x232a38)))
        })
        .on_click(on_click)
        .child(label.into())
}

impl Render for MainView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ctl = cx.global::<OverlayController>();
        let mode = ctl.mode;
        let topmost = ctl.topmost;
        let follow = ctl.follow.clone();
        let status = ctl.last_status.clone();
        let page = self.page;

        let nav = div().flex().flex_col().gap_1().children(Page::ALL.map(|p| {
            side_button(
                p.label(),
                p.label(),
                page == p,
                cx.listener(move |this, _, _, cx| {
                    this.page = p;
                    cx.notify();
                }),
            )
        }));

        let mode_buttons = div().flex().flex_col().gap_1().children(
            [
                ("mode-passive", OverlayMode::Passive, "Passive (穿透)"),
                (
                    "mode-interactive",
                    OverlayMode::Interactive,
                    "Interactive (可拖)",
                ),
                ("mode-hidden", OverlayMode::Hidden, "Hidden"),
            ]
            .map(|(id, m, label)| {
                side_button(
                    id,
                    label,
                    mode == m,
                    cx.listener(move |_, _, _, cx| {
                        overlay::set_mode(cx, m);
                        cx.notify();
                    }),
                )
            }),
        );

        let follow_buttons = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(side_button(
                "follow-none",
                "跟随: 关闭",
                follow == FollowTarget::None,
                cx.listener(|_, _, _, cx| {
                    overlay::set_follow(cx, FollowTarget::None);
                    cx.notify();
                }),
            ))
            .child(side_button(
                "follow-sim",
                "跟随: 模拟矩形",
                follow == FollowTarget::Simulated,
                cx.listener(|_, _, _, cx| {
                    overlay::set_follow(cx, FollowTarget::Simulated);
                    cx.notify();
                }),
            ))
            .child(side_button(
                "refresh-owners",
                format!("刷新窗口列表 ({})", self.window_owners.len()),
                false,
                cx.listener(|this, _, _, cx| {
                    this.refresh_owners();
                    cx.notify();
                }),
            ))
            .children(self.window_owners.iter().take(8).map(|owner| {
                let owner_cloned = owner.clone();
                let active = follow
                    == FollowTarget::Window {
                        owner: owner.clone(),
                    };
                div()
                    .id(gpui::SharedString::from(format!("owner-{owner}")))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .when(active, |el| el.bg(rgb(0x2b3446)).text_color(rgb(0xffd166)))
                    .when(!active, |el| {
                        el.text_color(rgb(0x9aa3b2))
                            .hover(|el| el.bg(rgb(0x232a38)))
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        overlay::set_follow(
                            cx,
                            FollowTarget::Window {
                                owner: owner_cloned.clone(),
                            },
                        );
                        cx.notify();
                    }))
                    .child(format!("跟随: {owner}"))
            }));

        let overlay_panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_3()
            .border_t_1()
            .border_color(rgb(0x2b3446))
            .child(
                div()
                    .text_color(rgb(0x9aa3b2))
                    .text_sm()
                    .child("Overlay 探针"),
            )
            .child(side_button(
                "overlay-create",
                "创建/显示 Overlay",
                false,
                cx.listener(|_, _, _, cx| {
                    overlay::ensure_overlay(cx);
                    cx.notify();
                }),
            ))
            .child(mode_buttons)
            .child(side_button(
                "topmost-toggle",
                if topmost {
                    "置顶: 开"
                } else {
                    "置顶: 关"
                },
                topmost,
                cx.listener(move |_, _, _, cx| {
                    overlay::set_topmost(cx, !topmost);
                    cx.notify();
                }),
            ))
            .child(follow_buttons)
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x7f8a9d))
                    .child(format!("状态: {status}")),
            );

        let sidebar = div()
            .flex()
            .flex_col()
            .w_64()
            .h_full()
            .p_3()
            .gap_2()
            .bg(rgb(0x181d27))
            .child(
                div()
                    .pb_2()
                    .text_color(rgb(0xffd166))
                    .child("Hearth Advisor"),
            )
            .child(nav)
            .child(div().flex_grow())
            .child(overlay_panel);

        let content = if page == Page::Inspector {
            div()
                .flex()
                .flex_grow()
                .h_full()
                .bg(rgb(0x1e2430))
                .text_color(rgb(0xe8ecf4))
                .child(self.inspector.clone())
        } else if page == Page::Live {
            div().flex().flex_grow().h_full().child(self.live.clone())
        } else if page == Page::Decks {
            div().flex().flex_grow().h_full().child(self.decks.clone())
        } else if page == Page::History {
            div()
                .flex()
                .flex_grow()
                .h_full()
                .child(self.history.clone())
        } else if page == Page::Pools {
            div().flex().flex_grow().h_full().child(self.pools.clone())
        } else if page == Page::Settings {
            self.render_settings(cx)
        } else {
            div()
                .flex()
                .flex_col()
                .flex_grow()
                .h_full()
                .justify_center()
                .items_center()
                .gap_2()
                .bg(rgb(0x1e2430))
                .child(div().text_color(rgb(0xe8ecf4)).child(page.label()))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgba(0xe8ecf480))
                        .child(page.placeholder()),
                )
        };

        div().flex().size_full().child(sidebar).child(content)
    }
}
