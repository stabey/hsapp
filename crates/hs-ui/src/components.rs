//! 基础组件(M2 轻量视觉基础)。纯展示,无领域状态(设计文档 §23-9)。

use gpui::{div, prelude::*, Div, SharedString, Stateful};

use crate::tokens as t;

/// 页面标题。
pub fn page_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(t::font_lg())
        .text_color(t::text_primary())
        .child(text.into())
}

/// 小节标题(次级)。
pub fn section_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(t::font_sm())
        .text_color(t::text_muted())
        .child(text.into())
}

/// 通用面板容器(浮起底色 + 圆角 + 内边距)。
pub fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(t::space_2())
        .p(t::space_3())
        .rounded_md()
        .bg(t::bg_panel())
}

/// 列表行容器(横向、垂直居中、行内间距)。
pub fn list_row() -> Div {
    div()
        .flex()
        .items_center()
        .gap(t::space_2())
        .px(t::space_2())
        .py(t::space_1())
        .rounded_sm()
        .text_size(t::font_sm())
        .text_color(t::text_primary())
}

/// 计数徽章(右对齐的小方块数字)。
pub fn badge(text: impl Into<SharedString>) -> Div {
    div()
        .px(t::space_2())
        .rounded_sm()
        .bg(t::bg_active())
        .text_size(t::font_xs())
        .text_color(t::accent())
        .child(text.into())
}

/// 可点击按钮(带 id;on_click 由调用方追加)。
pub fn button(id: &'static str, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .px(t::space_3())
        .py(t::space_1())
        .rounded_md()
        .cursor_pointer()
        .bg(t::bg_active())
        .text_size(t::font_sm())
        .text_color(t::text_primary())
        .hover(|el| el.bg(t::bg_hover()))
        .child(label.into())
}

/// 键值信息行(标签弱化、值正常)。
pub fn kv(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .gap(t::space_2())
        .text_size(t::font_sm())
        .child(
            div()
                .text_color(t::text_muted())
                .min_w(gpui::px(96.))
                .child(label.into()),
        )
        .child(div().text_color(t::text_primary()).child(value.into()))
}

/// 空状态占位(居中弱化文案)。
pub fn empty_state(text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_grow()
        .justify_center()
        .items_center()
        .text_size(t::font_sm())
        .text_color(t::text_faint())
        .child(text.into())
}
