//! `hs-overlay-platform` — Overlay 原生窗口行为的统一接口（设计文档 §14.2）。
//!
//! GPUI 只负责绘制；窗口级别、穿透、焦点、DPI 由本层的平台实现负责（ADR-006）。
//! 若 GPUI 无法提供可控原生句柄，本接口不变、仅替换实现（ADR-0005 降级路径）。

#[cfg(target_os = "macos")]
pub mod macos;

use hs_types::PhysicalRect;

/// Overlay 交互模式（设计文档 §14.3/§14.4）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayMode {
    /// 透明、置顶、鼠标穿透、不抢焦点（默认对局模式）
    Passive,
    /// 可点击、可拖动、显示边框（编辑模式）
    Interactive,
    Hidden,
}

/// 原生窗口句柄的跨平台包装。
/// macOS 下承载 NSWindow/NSPanel 指针，Windows 下承载 HWND；
/// G0 以不透明 usize 占位，Track A 实装时替换为 raw-window-handle 或平台类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWindowHandle(pub usize);

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("not implemented: {0}")]
    Unsupported(&'static str),
    #[error("invalid native handle")]
    InvalidHandle,
}

/// Overlay 平台契约（G0 冻结签名，Track A 实装 macOS 版本）。
pub trait OverlayPlatform {
    fn configure(&self, window: NativeWindowHandle) -> Result<(), OverlayError>;
    fn set_mode(&self, mode: OverlayMode) -> Result<(), OverlayError>;
    fn set_topmost(&self, enabled: bool) -> Result<(), OverlayError>;
    fn set_bounds(&self, bounds: PhysicalRect) -> Result<(), OverlayError>;
    fn set_visible(&self, visible: bool) -> Result<(), OverlayError>;
}
