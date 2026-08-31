//! `hs-diagnostics` — 组件健康状态与诊断（设计文档 §18）。

use serde::{Deserialize, Serialize};

/// 可观测组件清单（§18.1）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Component {
    GameDetection,
    LogLocation,
    LogTail,
    Parser,
    StateReducer,
    CardDatabase,
    ImageCache,
    Overlay,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ComponentHealth {
    Healthy,
    Degraded { reason: String },
    Failed { reason: String },
}
