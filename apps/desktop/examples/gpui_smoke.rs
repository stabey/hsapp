//! G0-02 GPUI 冒烟验证：确认锁定版本在当前工具链/macOS 下可编译、可弹窗。
//! 运行：cargo run -p desktop --example gpui_smoke

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Window, WindowBounds,
    WindowOptions,
};

struct Smoke;

impl Render for Smoke {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0x1e2430))
            .child(
                div()
                    .text_color(rgb(0xffd166))
                    .child("Hearth Advisor — GPUI smoke OK"),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(480.), px(240.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Smoke),
        )
        .expect("open window");
        cx.activate(true);
    });
}
