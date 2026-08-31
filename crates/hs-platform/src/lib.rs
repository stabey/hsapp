//! `hs-platform` — Hearthstone 安装/进程/窗口/日志定位的平台抽象（设计文档 §7.1）。
//!
//! 平台私有实现集中在本 crate 的 `macos`（及未来 `windows`）子模块，
//! 禁止散落到 UI 或其他 crate（ADR-006）。

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_window;

mod app_dirs;
mod log_config;
mod log_locate;

pub use app_dirs::{
    app_data_dir, bundle_resources_dir, bundle_resources_dir_for_exe,
    default_app_data_dir_for_home, default_card_db_path, DATA_DIR_ENV,
};
pub use log_config::{
    ensure_log_config, validate_log_config, validate_log_config_str, LogConfigWriteReport,
    ENSURED_POWER_ENTRIES, REQUIRED_POWER_KEYS,
};
pub use log_locate::{
    collect_log_candidates, default_macos_log_config_path, default_macos_log_dirs,
    pick_active_candidate, LogLocateOptions, SESSION_LOG_MARKERS, SYSTEM_INSTALL_LOG_DIRS,
};

use hs_types::PhysicalRect;
use std::path::PathBuf;

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GameInstallation {
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LogCandidate {
    /// 日志目录（含按会话分目录的新版布局）。
    pub dir: PathBuf,
    /// log.config 路径（健康检查用，见 M0 拆解 D-03）。
    pub log_config: Option<PathBuf>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GameProcess {
    pub pid: u32,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GameWindow {
    /// 物理像素边界（转逻辑坐标走 hs_types::ScaleFactor）。
    pub bounds: PhysicalRect,
    pub minimized: bool,
    pub on_screen: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("not implemented on this platform: {0}")]
    Unsupported(&'static str),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 平台定位契约（设计文档 §7.1；G0 冻结签名）。
pub trait HearthstonePlatform {
    fn locate_installations(&self) -> Result<Vec<GameInstallation>, PlatformError>;
    fn locate_log_candidates(&self) -> Result<Vec<LogCandidate>, PlatformError>;
    fn detect_process(&self) -> Result<Option<GameProcess>, PlatformError>;
    fn find_game_window(&self, process: &GameProcess) -> Result<Option<GameWindow>, PlatformError>;
}
