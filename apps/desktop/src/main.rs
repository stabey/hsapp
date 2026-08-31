//! Hearth Advisor 桌面主程序。
//! 主窗口(导航 + 页面)、共享 GameSession、透明 Overlay。
//! 目前仅支持 macOS;Windows 入口在 M4 接入。

mod advice_presenter;
mod advisor_resources;
pub mod advisor_session;
mod cards;
mod decks_watch;
mod inspector;
mod main_window;
mod overlay;
mod pages;
mod pool_query;
mod rules_store;
mod session;
mod stores;

use gpui::{px, size, App, AppContext as _, Application, Bounds, WindowBounds, WindowOptions};

use main_window::MainView;
use overlay::OverlayController;
use stores::Stores;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("hearth-advisor desktop starting");

    Application::new().run(|cx: &mut App| {
        cx.set_global(OverlayController::new());
        // 牌组/历史仓储:打开失败不阻塞其余功能,页面显示空态与原因。
        let stores = Stores::open_default();
        if let Some(err) = &stores.open_error {
            tracing::warn!("deck store unavailable: {err}");
        }
        cx.set_global(stores);
        // 卡牌显示名（dbf/card_id → 中文名）；空库时降级为原始 ID。
        let names = cards::CardNames::open_default();
        if let Some(err) = &names.open_error {
            tracing::warn!("card db unavailable: {err}");
        }
        cx.set_global(names);
        // 规则/池定义(M3):加载失败不阻塞,Pools 页显示错误明细。
        let rules = rules_store::RulesStore::load_default();
        for err in &rules.errors {
            tracing::warn!("rules store: {err}");
        }
        cx.set_global(rules);
        // 先吃完 Decks.log 时间线,再跟 Power.log,对局才能按排队牌组落库。
        decks_watch::init(cx);
        let session = session::init(cx);
        // 建立唯一建议调度实体；Live 与延迟创建的 Overlay 复用同一 Handle。
        let advice = advisor_session::init(session.clone(), cx);
        let bounds = Bounds::centered(None, size(px(1080.), px(680.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                let inspector = inspector::build(session.clone(), cx);
                let live =
                    cx.new(|cx| pages::live::LiveView::new(session.clone(), advice.clone(), cx));
                let decks = cx.new(|_| pages::decks::DecksView::new());
                let history = cx.new(|_| pages::history::HistoryView::new());
                let pools = cx.new(|_| pages::pools::PoolsView::new());
                cx.new(|_| MainView::new(inspector, live, decks, history, pools))
            },
        )
        .expect("open main window");
        cx.activate(true);
    });
    Ok(())
}
