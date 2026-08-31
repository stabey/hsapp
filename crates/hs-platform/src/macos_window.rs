//! macOS 窗口枚举（M0 Track A，TA-05）。
//!
//! 基于 `CGWindowListCopyWindowInfo`，只读取 owner 名/PID/层级/边界，
//! 刻意不读 `kCGWindowName`（读取窗口标题在现代 macOS 上需要屏幕录制权限）。
//!
//! 坐标说明：CGWindowList 返回的边界是**全局 point 坐标、顶左原点**
//! （非物理像素），因此这里用 `LogicalRect` 承载；换算物理像素由调用方
//! 结合目标显示器的 `ScaleFactor` 完成。

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::geometry::CGRect;
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowNumber,
    kCGWindowOwnerName, kCGWindowOwnerPID,
};

use hs_types::LogicalRect;

use crate::PlatformError;

/// 屏幕上一个可跟随的候选窗口。
#[derive(Clone, Debug)]
pub struct OnScreenWindow {
    pub window_id: u32,
    pub owner_pid: i32,
    /// 进程名（如 "Hearthstone"）。读取它不需要任何 TCC 权限。
    pub owner_name: String,
    /// 窗口层级；0 为普通应用窗口，菜单栏/Dock 等系统元素为非 0。
    pub layer: i32,
    /// 全局 point 坐标（顶左原点）。
    pub bounds_points: LogicalRect,
}

/// 枚举屏幕上的普通应用窗口（layer == 0，排除桌面元素）。
pub fn list_on_screen_windows() -> Result<Vec<OnScreenWindow>, PlatformError> {
    let info = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
    .ok_or(PlatformError::Unsupported(
        "CGWindowListCopyWindowInfo returned null",
    ))?;

    let mut windows = Vec::new();
    for item in info.iter() {
        let dict =
            unsafe { CFDictionary::<CFString, CFType>::wrap_under_get_rule(*item as *const _) };
        let Some(win) = parse_window(&dict) else {
            continue;
        };
        if win.layer == 0 {
            windows.push(win);
        }
    }
    Ok(windows)
}

/// 按进程名（不区分大小写包含匹配）过滤屏幕窗口。
pub fn find_windows_by_owner(owner: &str) -> Result<Vec<OnScreenWindow>, PlatformError> {
    let needle = owner.to_lowercase();
    Ok(list_on_screen_windows()?
        .into_iter()
        .filter(|w| w.owner_name.to_lowercase().contains(&needle))
        .collect())
}

fn parse_window(dict: &CFDictionary<CFString, CFType>) -> Option<OnScreenWindow> {
    unsafe {
        let key_number = CFString::wrap_under_get_rule(kCGWindowNumber);
        let key_pid = CFString::wrap_under_get_rule(kCGWindowOwnerPID);
        let key_layer = CFString::wrap_under_get_rule(kCGWindowLayer);
        let key_owner = CFString::wrap_under_get_rule(kCGWindowOwnerName);
        let key_bounds = CFString::wrap_under_get_rule(kCGWindowBounds);

        let window_id = dict_i64(dict, &key_number)? as u32;
        let owner_pid = dict_i64(dict, &key_pid)? as i32;
        let layer = dict_i64(dict, &key_layer)? as i32;
        let owner_name = dict
            .find(&key_owner)
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let bounds_ref = dict.find(&key_bounds)?;
        let bounds_dict = bounds_ref.downcast::<CFDictionary>()?;
        let rect = CGRect::from_dict_representation(&bounds_dict)?;

        Some(OnScreenWindow {
            window_id,
            owner_pid,
            owner_name,
            layer,
            bounds_points: LogicalRect {
                x: rect.origin.x as f32,
                y: rect.origin.y as f32,
                width: rect.size.width as f32,
                height: rect.size.height as f32,
            },
        })
    }
}

fn dict_i64(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i64> {
    dict.find(key)
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_i64())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无头 CI 下也应能调用而不崩溃（可能返回空列表）。
    #[test]
    fn list_windows_does_not_crash() {
        let result = list_on_screen_windows();
        match result {
            Ok(windows) => {
                for w in &windows {
                    assert_eq!(w.layer, 0);
                }
            }
            Err(PlatformError::Unsupported(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
