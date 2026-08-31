//! 设计 token(M2 轻量视觉基础,PRD 决策 2)。
//!
//! 唯一取色/间距来源:desktop 与 Overlay 一律引用这里,禁止散落魔法数。
//! 色值沿用 M0 开发者壳子的既有调色板,先统一再演进。

use gpui::{px, rgb, rgba, Pixels, Rgba};

/// 应用最外层背景(侧栏)。
pub fn bg_app() -> Rgba {
    rgb(0x181d27)
}

/// 内容区背景。
pub fn bg_content() -> Rgba {
    rgb(0x1e2430)
}

/// 面板/卡片底色(略浮起)。
pub fn bg_panel() -> Rgba {
    rgb(0x232a38)
}

/// 选中/激活底色。
pub fn bg_active() -> Rgba {
    rgb(0x2b3446)
}

/// hover 底色。
pub fn bg_hover() -> Rgba {
    rgb(0x35425c)
}

/// 主文本。
pub fn text_primary() -> Rgba {
    rgb(0xe8ecf4)
}

/// 次级文本(说明/标签)。
pub fn text_muted() -> Rgba {
    rgb(0x9aa3b2)
}

/// 弱化文本(状态行/水印)。
pub fn text_faint() -> Rgba {
    rgb(0x7f8a9d)
}

/// 品牌/强调色(标题、激活项文字)。
pub fn accent() -> Rgba {
    rgb(0xffd166)
}

/// 成功/健康。
pub fn ok() -> Rgba {
    rgb(0x7bd88f)
}

/// 警告。
pub fn warn() -> Rgba {
    rgb(0xffd166)
}

/// 错误/失败。
pub fn err() -> Rgba {
    rgb(0xef6b73)
}

/// 分隔线/描边。
pub fn border() -> Rgba {
    rgb(0x2b3446)
}

/// Overlay 面板半透明底(游戏画面上要能透出背景)。
pub fn overlay_panel_bg() -> Rgba {
    rgba(0x181d27d9)
}

/// Overlay 行 hover/强调底。
pub fn overlay_row_bg() -> Rgba {
    rgba(0x232a38cc)
}

/// 间距刻度(逻辑像素)。用刻度不用裸数,保证节奏一致。
pub fn space_1() -> Pixels {
    px(4.)
}
pub fn space_2() -> Pixels {
    px(8.)
}
pub fn space_3() -> Pixels {
    px(12.)
}
pub fn space_4() -> Pixels {
    px(16.)
}
pub fn space_6() -> Pixels {
    px(24.)
}

/// 字号刻度。
pub fn font_xs() -> Pixels {
    px(11.)
}
pub fn font_sm() -> Pixels {
    px(13.)
}
pub fn font_md() -> Pixels {
    px(15.)
}
pub fn font_lg() -> Pixels {
    px(18.)
}
