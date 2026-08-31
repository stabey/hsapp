//! macOS 日志目录 / log.config 路径收集（TC-03）。
//!
//! 默认路径来自 HearthSim / HSTracker 惯例：
//! - log.config: `~/Library/Preferences/Blizzard/Hearthstone/log.config`
//! - 日志目录: `~/Library/Logs/Hearthstone`、
//!   `~/Library/Application Support/Blizzard/Hearthstone/Logs`、
//!   以及（可选）`/Applications/Hearthstone/Logs`
//!
//! 手动指定通过 `extra_dirs` / `extra_log_config`（或环境变量
//! `HS_LOG_DIR` / `HS_LOG_CONFIG`，由 `MacPlatform` 读入）。

use crate::LogCandidate;
use std::path::{Path, PathBuf};

/// 系统安装目录下的 Logs（不随 `$HOME` 变化，测试默认关闭以免扫到真机）。
pub const SYSTEM_INSTALL_LOG_DIRS: &[&str] = &["/Applications/Hearthstone/Logs"];

/// 能证明"炉石在此目录写日志"的标记文件（任一存在即算）。
/// 注意开局前会话目录里只有 Decks.log / Hearthstone.log,还没有 Power.log。
pub const SESSION_LOG_MARKERS: &[&str] = &["Power.log", "Decks.log", "Hearthstone.log"];

pub struct LogLocateOptions<'a> {
    pub home: &'a Path,
    pub extra_dirs: &'a [PathBuf],
    pub extra_log_config: Option<&'a Path>,
    pub include_system_installs: bool,
}

pub fn default_macos_log_config_path(home: &Path) -> PathBuf {
    home.join("Library/Preferences/Blizzard/Hearthstone/log.config")
}

pub fn default_macos_log_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Library/Logs/Hearthstone"),
        home.join("Library/Application Support/Blizzard/Hearthstone/Logs"),
    ]
}

/// 收集日志候选目录。手动目录即使尚不存在也会返回；默认目录始终包含主路径。
pub fn collect_log_candidates(opts: LogLocateOptions<'_>) -> Vec<LogCandidate> {
    let log_config = Some(
        opts.extra_log_config
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_macos_log_config_path(opts.home)),
    );

    let mut roots = Vec::new();
    let defaults = default_macos_log_dirs(opts.home);
    roots.extend(defaults);
    if opts.include_system_installs {
        roots.extend(SYSTEM_INSTALL_LOG_DIRS.iter().map(PathBuf::from));
    }
    roots.extend(opts.extra_dirs.iter().cloned());

    let mut out = Vec::new();
    for (idx, root) in roots.into_iter().enumerate() {
        let is_primary_default = idx == 0;
        let is_manual = opts.extra_dirs.iter().any(|d| d == &root);
        for dir in expand_log_root(&root, is_primary_default || is_manual) {
            push_unique(&mut out, dir, log_config.clone());
        }
    }
    out
}

fn expand_log_root(root: &Path, keep_if_missing: bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if latest_marker_mtime(root).is_some() {
        dirs.push(root.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        let mut sessions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && latest_marker_mtime(p).is_some())
            .collect();
        sessions.sort();
        dirs.extend(sessions);
    }
    if dirs.is_empty() && (keep_if_missing || root.is_dir()) {
        dirs.push(root.to_path_buf());
    }
    dirs
}

/// 目录下标记日志文件的最新修改时间;没有任何标记文件返回 `None`。
fn latest_marker_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    SESSION_LOG_MARKERS
        .iter()
        .filter_map(|name| {
            std::fs::metadata(dir.join(name))
                .ok()
                .and_then(|m| m.modified().ok())
        })
        .max()
}

/// 从候选中挑"当前活跃"的日志目录:标记日志文件 mtime 最新者胜出
/// (炉石每次启动新建会话目录,最新写入的就是当前会话)。
/// 所有候选都没有标记文件时退回第一个候选,保持旧行为。
pub fn pick_active_candidate(candidates: &[LogCandidate]) -> Option<&LogCandidate> {
    candidates
        .iter()
        .filter_map(|c| latest_marker_mtime(&c.dir).map(|t| (t, c)))
        .max_by_key(|(t, _)| *t)
        .map(|(_, c)| c)
        .or_else(|| candidates.first())
}

fn push_unique(out: &mut Vec<LogCandidate>, dir: PathBuf, log_config: Option<PathBuf>) {
    if out.iter().any(|c| c.dir == dir) {
        return;
    }
    out.push(LogCandidate { dir, log_config });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn default_home_layout_and_manual_override() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        touch(&home.join("Library/Logs/Hearthstone/Power.log"));
        touch(&home.join("Library/Preferences/Blizzard/Hearthstone/log.config"));

        let extra = tmp.path().join("manual-logs");
        fs::create_dir_all(&extra).unwrap();
        touch(&extra.join("Power.log"));

        let extra_config = tmp.path().join("custom.log.config");
        fs::write(&extra_config, b"[Power]\nLogLevel=1\nFilePrinting=true\n").unwrap();

        let found = collect_log_candidates(LogLocateOptions {
            home,
            extra_dirs: std::slice::from_ref(&extra),
            extra_log_config: Some(&extra_config),
            include_system_installs: false,
        });

        assert!(
            found
                .iter()
                .any(|c| c.dir == home.join("Library/Logs/Hearthstone")),
            "{found:?}"
        );
        assert!(found.iter().any(|c| c.dir == extra), "{found:?}");
        assert!(found
            .iter()
            .all(|c| c.log_config.as_deref() == Some(extra_config.as_path())));
    }

    #[test]
    fn session_subdirectories_are_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let logs = home.join("Library/Logs/Hearthstone");
        touch(&logs.join("Hearthstone_2026_01_01/Power.log"));
        touch(&logs.join("Hearthstone_2026_01_02/Power.log"));

        let found = collect_log_candidates(LogLocateOptions {
            home,
            extra_dirs: &[],
            extra_log_config: None,
            include_system_installs: false,
        });
        let dirs: Vec<_> = found.iter().map(|c| c.dir.clone()).collect();
        assert!(
            dirs.contains(&logs.join("Hearthstone_2026_01_01")),
            "{dirs:?}"
        );
        assert!(
            dirs.contains(&logs.join("Hearthstone_2026_01_02")),
            "{dirs:?}"
        );
        assert_eq!(
            found[0].log_config.as_deref(),
            Some(default_macos_log_config_path(home).as_path())
        );
    }

    #[test]
    fn session_with_only_decks_log_is_candidate() {
        // 开局前的新会话目录只有 Decks.log(还没有 Power.log),
        // 牌组自动导入必须能定位到它。
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let logs = home.join("Library/Logs/Hearthstone");
        touch(&logs.join("Hearthstone_2026_08_22/Decks.log"));

        let found = collect_log_candidates(LogLocateOptions {
            home,
            extra_dirs: &[],
            extra_log_config: None,
            include_system_installs: false,
        });
        let dirs: Vec<_> = found.iter().map(|c| c.dir.clone()).collect();
        assert!(
            dirs.contains(&logs.join("Hearthstone_2026_08_22")),
            "{dirs:?}"
        );
    }

    #[test]
    fn pick_active_prefers_newest_marker_over_first_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let logs = home.join("Library/Logs/Hearthstone");
        let old_session = logs.join("Hearthstone_2026_08_21");
        let new_session = logs.join("Hearthstone_2026_08_22");
        touch(&old_session.join("Power.log"));
        touch(&new_session.join("Decks.log"));

        // 显式拉开 mtime,避免文件系统时间粒度导致两目录同时刻。
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(old_session.join("Power.log"))
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        let found = collect_log_candidates(LogLocateOptions {
            home,
            extra_dirs: &[],
            extra_log_config: None,
            include_system_installs: false,
        });
        // 第一个候选是旧会话(按名字升序),但活跃挑选要选 mtime 最新的
        let picked = pick_active_candidate(&found).expect("candidates non-empty");
        assert_eq!(picked.dir, new_session, "{found:?}");
    }

    #[test]
    fn pick_active_falls_back_to_first_when_no_markers() {
        let candidates = vec![
            LogCandidate {
                dir: PathBuf::from("/nonexistent/a"),
                log_config: None,
            },
            LogCandidate {
                dir: PathBuf::from("/nonexistent/b"),
                log_config: None,
            },
        ];
        let picked = pick_active_candidate(&candidates).unwrap();
        assert_eq!(picked.dir, PathBuf::from("/nonexistent/a"));
        assert!(pick_active_candidate(&[]).is_none());
    }

    #[test]
    fn missing_default_dir_still_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let found = collect_log_candidates(LogLocateOptions {
            home,
            extra_dirs: &[],
            extra_log_config: None,
            include_system_installs: false,
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, home.join("Library/Logs/Hearthstone"));
    }
}
