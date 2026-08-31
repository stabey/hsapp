//! macOS 实现（M0 Track A/C 实装；G0 提供显式未实现占位）。

use crate::{
    collect_log_candidates, GameInstallation, GameProcess, GameWindow, HearthstonePlatform,
    LogCandidate, LogLocateOptions, PlatformError,
};
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct MacPlatform;

impl HearthstonePlatform for MacPlatform {
    fn locate_installations(&self) -> Result<Vec<GameInstallation>, PlatformError> {
        Err(PlatformError::Unsupported(
            "MacPlatform::locate_installations (Track C)",
        ))
    }

    fn locate_log_candidates(&self) -> Result<Vec<LogCandidate>, PlatformError> {
        let home = match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home),
            None => {
                return Err(
                    std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set").into(),
                );
            }
        };
        let extra_dirs: Vec<PathBuf> = std::env::var_os("HS_LOG_DIR")
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let extra_log_config = std::env::var_os("HS_LOG_CONFIG").map(PathBuf::from);
        Ok(collect_log_candidates(LogLocateOptions {
            home: &home,
            extra_dirs: &extra_dirs,
            extra_log_config: extra_log_config.as_deref(),
            include_system_installs: true,
        }))
    }

    fn detect_process(&self) -> Result<Option<GameProcess>, PlatformError> {
        Err(PlatformError::Unsupported(
            "MacPlatform::detect_process (Track C)",
        ))
    }

    fn find_game_window(
        &self,
        _process: &GameProcess,
    ) -> Result<Option<GameWindow>, PlatformError> {
        Err(PlatformError::Unsupported(
            "MacPlatform::find_game_window (Track A)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_log_candidates_is_implemented() {
        let found = MacPlatform
            .locate_log_candidates()
            .expect("HOME should be set in the test environment");
        assert!(
            found
                .iter()
                .any(|c| c.dir.ends_with("Library/Logs/Hearthstone")),
            "{found:?}"
        );
        assert!(found.iter().all(|c| c.log_config.is_some()), "{found:?}");
    }
}
