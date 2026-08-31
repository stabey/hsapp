//! macOS `OverlayPlatform` 实现（M0 Track A，TA-02/TA-03）。
//!
//! GPUI 以 `WindowKind::PopUp` 创建的窗口本身已是 NSPanel 子类，并带有
//! `NonactivatingPanel` styleMask、`NSPopUpWindowLevel`、
//! `CanJoinAllSpaces | FullScreenAuxiliary` collectionBehavior。
//! 本实现只在其上追加 Overlay 需要、GPUI 未暴露的行为：
//! 鼠标穿透、层级切换、背景拖动、显隐、以及 setFrame 跟随。
//!
//! 坐标约定：`set_bounds` 接收 **物理像素、全局顶左原点**（CG 截屏坐标系乘以
//! 窗口所在屏的 backingScaleFactor），内部换算为 Cocoa 的底左原点 point 坐标。

#![allow(unexpected_cfgs)] // objc 0.2 宏内部 cfg(verify_message) 触发的误报

use std::cell::Cell;
use std::ffi::c_void;

use hs_types::{LogicalRect, PhysicalRect};
use objc::runtime::{Object, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl};

use crate::{NativeWindowHandle, OverlayError, OverlayMode, OverlayPlatform};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// NSWindow level 常量（CGWindowLevelKey 换算后的常用值）
const LEVEL_NORMAL: i64 = 0;
const LEVEL_POPUP: i64 = 101;
/// 盖过全屏游戏窗口所需层级（ScreenSaver 级）。
const LEVEL_SCREEN_SAVER: i64 = 1000;

/// `NSWindowCollectionBehaviorStationary`：Exposé/Mission Control 中不移动。
const COLLECTION_BEHAVIOR_STATIONARY: u64 = 1 << 4;

/// 由模式唯一决定的输入与可见性策略。
///
/// 这层保持纯值，既避免 Advice 等纯渲染内容误改穿透语义，也让 AppKit
/// 之外的测试能锁定 Passive/Interactive/Hidden 的安全边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OverlayModePolicy {
    ignores_mouse_events: bool,
    movable_by_background: bool,
    visible: bool,
}

fn mode_policy(mode: OverlayMode) -> OverlayModePolicy {
    match mode {
        OverlayMode::Passive => OverlayModePolicy {
            ignores_mouse_events: true,
            movable_by_background: false,
            visible: true,
        },
        OverlayMode::Interactive => OverlayModePolicy {
            ignores_mouse_events: false,
            movable_by_background: true,
            visible: true,
        },
        OverlayMode::Hidden => OverlayModePolicy {
            ignores_mouse_events: true,
            movable_by_background: false,
            visible: false,
        },
    }
}

/// macOS Overlay 平台实现。非线程安全，仅限主线程使用（AppKit 约束）。
pub struct MacOverlayPlatform {
    window: Cell<Option<NativeWindowHandle>>,
    topmost: Cell<bool>,
    mode: Cell<OverlayMode>,
}

impl Default for MacOverlayPlatform {
    fn default() -> Self {
        Self {
            window: Cell::new(None),
            topmost: Cell::new(true),
            mode: Cell::new(OverlayMode::Hidden),
        }
    }
}

impl MacOverlayPlatform {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 raw-window-handle 的 `ns_view` 指针取 NSWindow，包装为句柄。
    ///
    /// # Safety
    /// `ns_view` 必须是有效的 NSView 指针且窗口仍存活。
    pub unsafe fn handle_from_ns_view(ns_view: *mut c_void) -> Option<NativeWindowHandle> {
        if ns_view.is_null() {
            return None;
        }
        let view = ns_view as *mut Object;
        let window: *mut Object = msg_send![view, window];
        if window.is_null() {
            None
        } else {
            Some(NativeWindowHandle(window as usize))
        }
    }

    fn ns_window(&self) -> Result<*mut Object, OverlayError> {
        let handle = self.window.get().ok_or(OverlayError::InvalidHandle)?;
        if handle.0 == 0 {
            return Err(OverlayError::InvalidHandle);
        }
        Ok(handle.0 as *mut Object)
    }

    fn apply_level(&self, win: *mut Object) {
        let level = if !self.topmost.get() {
            LEVEL_NORMAL
        } else if self.mode.get() == OverlayMode::Interactive {
            LEVEL_POPUP
        } else {
            LEVEL_SCREEN_SAVER
        };
        unsafe {
            let _: () = msg_send![win, setLevel: level];
        }
    }

    /// 以全局 point 坐标（顶左原点）直接设置 frame。
    /// CGWindowList 的窗口边界就是这种坐标，跟随场景用它可避免
    /// 物理像素的往返换算误差。
    pub fn set_frame_points(&self, rect: LogicalRect) -> Result<(), OverlayError> {
        let win = self.ns_window()?;
        unsafe {
            let screen_h = primary_screen_height_points();
            let frame = CGRect {
                origin: CGPoint {
                    x: f64::from(rect.x),
                    y: screen_h - f64::from(rect.y) - f64::from(rect.height),
                },
                size: CGSize {
                    width: f64::from(rect.width),
                    height: f64::from(rect.height),
                },
            };
            let display: BOOL = YES;
            let _: () = msg_send![win, setFrame: frame display: display];
        }
        Ok(())
    }

    /// 当前 frame（全局 point 坐标、顶左原点），供位置保存与 UI 展示。
    pub fn current_frame_points(&self) -> Result<LogicalRect, OverlayError> {
        let win = self.ns_window()?;
        unsafe {
            let frame: CGRect = msg_send![win, frame];
            let screen_h = primary_screen_height_points();
            Ok(LogicalRect {
                x: frame.origin.x as f32,
                y: (screen_h - frame.origin.y - frame.size.height) as f32,
                width: frame.size.width as f32,
                height: frame.size.height as f32,
            })
        }
    }

    /// 窗口 backingScaleFactor（Retina=2.0）。
    pub fn scale_factor(&self) -> Result<f64, OverlayError> {
        let win = self.ns_window()?;
        unsafe { Ok(msg_send![win, backingScaleFactor]) }
    }

    /// 当前窗口 frame（物理像素、全局顶左原点），供位置保存使用。
    pub fn current_bounds(&self) -> Result<PhysicalRect, OverlayError> {
        let win = self.ns_window()?;
        unsafe {
            let frame: CGRect = msg_send![win, frame];
            let scale: f64 = msg_send![win, backingScaleFactor];
            let screen_h = primary_screen_height_points();
            // Cocoa 底左 → CG 顶左
            let top = screen_h - (frame.origin.y + frame.size.height);
            Ok(PhysicalRect {
                x: (frame.origin.x * scale) as i32,
                y: (top * scale) as i32,
                width: (frame.size.width * scale) as u32,
                height: (frame.size.height * scale) as u32,
            })
        }
    }
}

/// 主屏高度（point），用于顶左/底左坐标互换。
fn primary_screen_height_points() -> f64 {
    unsafe {
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return 0.0;
        }
        // Cocoa 全局坐标以 screens[0]（含菜单栏的主屏）为基准
        let primary: *mut Object = msg_send![screens, objectAtIndex: 0usize];
        let frame: CGRect = msg_send![primary, frame];
        frame.size.height
    }
}

impl OverlayPlatform for MacOverlayPlatform {
    fn configure(&self, window: NativeWindowHandle) -> Result<(), OverlayError> {
        if window.0 == 0 {
            return Err(OverlayError::InvalidHandle);
        }
        self.window.set(Some(window));
        let win = self.ns_window()?;
        unsafe {
            // NSPanel 默认 hidesOnDeactivate=YES：应用失活（玩家点回游戏）时
            // 面板会消失，对 Overlay 是致命行为，必须关闭。
            let _: () = msg_send![win, setHidesOnDeactivate: NO];
            let _: () = msg_send![win, setHasShadow: NO];
            let _: () = msg_send![win, setOpaque: NO];
            let behavior: u64 = msg_send![win, collectionBehavior];
            let _: () =
                msg_send![win, setCollectionBehavior: behavior | COLLECTION_BEHAVIOR_STATIONARY];
        }
        self.set_mode(self.mode.get())
    }

    fn set_mode(&self, mode: OverlayMode) -> Result<(), OverlayError> {
        let win = self.ns_window()?;
        self.mode.set(mode);
        let policy = mode_policy(mode);
        unsafe {
            let ignores_mouse: BOOL = if policy.ignores_mouse_events { YES } else { NO };
            let movable: BOOL = if policy.movable_by_background {
                YES
            } else {
                NO
            };
            let _: () = msg_send![win, setIgnoresMouseEvents: ignores_mouse];
            let _: () = msg_send![win, setMovableByWindowBackground: movable];
            if policy.visible {
                self.apply_level(win);
                let _: () = msg_send![win, orderFrontRegardless];
            } else {
                let nil: *mut Object = std::ptr::null_mut();
                let _: () = msg_send![win, orderOut: nil];
            }
        }
        Ok(())
    }

    fn set_topmost(&self, enabled: bool) -> Result<(), OverlayError> {
        let win = self.ns_window()?;
        self.topmost.set(enabled);
        self.apply_level(win);
        Ok(())
    }

    fn set_bounds(&self, bounds: PhysicalRect) -> Result<(), OverlayError> {
        let win = self.ns_window()?;
        unsafe {
            let scale: f64 = msg_send![win, backingScaleFactor];
            let scale = if scale > 0.0 { scale } else { 1.0 };
            let screen_h = primary_screen_height_points();
            let w = f64::from(bounds.width) / scale;
            let h = f64::from(bounds.height) / scale;
            let x = f64::from(bounds.x) / scale;
            // CG 顶左 → Cocoa 底左
            let y = screen_h - (f64::from(bounds.y) / scale + h);
            let frame = CGRect {
                origin: CGPoint { x, y },
                size: CGSize {
                    width: w,
                    height: h,
                },
            };
            let display: BOOL = YES;
            let _: () = msg_send![win, setFrame: frame display: display];
        }
        Ok(())
    }

    fn set_visible(&self, visible: bool) -> Result<(), OverlayError> {
        let win = self.ns_window()?;
        unsafe {
            if visible {
                let _: () = msg_send![win, orderFrontRegardless];
            } else {
                let nil: *mut Object = std::ptr::null_mut();
                let _: () = msg_send![win, orderOut: nil];
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_mode_is_visible_and_fully_click_through() {
        assert_eq!(
            mode_policy(OverlayMode::Passive),
            OverlayModePolicy {
                ignores_mouse_events: true,
                movable_by_background: false,
                visible: true,
            }
        );
    }

    #[test]
    fn interactive_mode_only_enables_window_editing() {
        assert_eq!(
            mode_policy(OverlayMode::Interactive),
            OverlayModePolicy {
                ignores_mouse_events: false,
                movable_by_background: true,
                visible: true,
            }
        );
    }

    #[test]
    fn hidden_mode_is_noninteractive_and_not_visible() {
        assert_eq!(
            mode_policy(OverlayMode::Hidden),
            OverlayModePolicy {
                ignores_mouse_events: true,
                movable_by_background: false,
                visible: false,
            }
        );
    }
}
