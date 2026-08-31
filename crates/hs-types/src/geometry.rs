//! 坐标三件套（设计文档 §8.4）：
//! 禁止用一个含义不明的 Rect 同时表示 macOS Point、Windows 逻辑像素和物理像素。

use serde::{Deserialize, Serialize};

/// 物理像素矩形（显示器原生像素，Win32/CGWindow 边界用）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 逻辑坐标矩形（DPI 无关，UI 布局与锚点存储用）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogicalRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// 物理/逻辑换算因子（Retina=2.0，Windows 125%=1.25）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScaleFactor(pub f64);

impl PhysicalRect {
    /// 按缩放因子转换为逻辑坐标。
    pub fn to_logical(self, scale: ScaleFactor) -> LogicalRect {
        let s = scale.0 as f32;
        LogicalRect {
            x: self.x as f32 / s,
            y: self.y as f32 / s,
            width: self.width as f32 / s,
            height: self.height as f32 / s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_to_logical_retina() {
        let phys = PhysicalRect {
            x: 200,
            y: 100,
            width: 3024,
            height: 1964,
        };
        let log = phys.to_logical(ScaleFactor(2.0));
        assert_eq!(log.x, 100.0);
        assert_eq!(log.y, 50.0);
        assert_eq!(log.width, 1512.0);
        assert_eq!(log.height, 982.0);
    }
}
