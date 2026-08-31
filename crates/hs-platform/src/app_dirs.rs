//! 应用数据目录(M1 S1,数据目录统一)与 .app bundle 内的只读资源目录。
//!
//! 所有落盘产物(卡牌库、后续对局产物)统一放在 `app_data_dir()` 下;
//! `HS_DATA_DIR` 环境变量可整体覆盖(测试/CI 用)。
//!
//! 随包分发的只读资源(规则 YAML 等)走 `bundle_resources_dir()`:
//! 打包成 .app 后进程 CWD 不再是仓库根,只能相对可执行文件定位。

use std::path::{Path, PathBuf};

/// 覆盖应用数据目录的环境变量。
pub const DATA_DIR_ENV: &str = "HS_DATA_DIR";

/// 给定 home 时的平台默认数据目录(macOS 惯例)。
pub fn default_app_data_dir_for_home(home: &Path) -> PathBuf {
    home.join("Library/Application Support/HearthAdvisor")
}

/// 应用数据目录:`HS_DATA_DIR` 优先;否则 `$HOME` 下的平台默认;
/// `$HOME` 缺失时退回进程 cwd 下的 `.hearth-advisor`(仅测试/异常环境)。
pub fn app_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => default_app_data_dir_for_home(Path::new(&home)),
        _ => PathBuf::from(".hearth-advisor"),
    }
}

/// 卡牌库 SQLite 的统一默认路径。
pub fn default_card_db_path() -> PathBuf {
    app_data_dir().join("cards.sqlite")
}

/// 可执行文件位于 `<name>.app/Contents/MacOS/` 时,返回同包的 `Contents/Resources`。
///
/// 纯函数(不碰文件系统与环境),便于测试;取真实进程路径用
/// [`bundle_resources_dir`]。
///
/// 三层结构必须全中:只认 `MacOS` + `Contents` 两层的话,任意
/// `~/tmp/Contents/MacOS/` 都会被误判成 bundle,程序转而去一个不存在的
/// Resources 找资源再静默降级——正是 bundle 化要根除的那类问题。
pub fn bundle_resources_dir_for_exe(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if !name_eq(macos_dir.file_name(), "MacOS") {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if !name_eq(contents_dir.file_name(), "Contents") {
        return None;
    }
    if !name_eq(contents_dir.parent()?.extension(), "app") {
        return None;
    }
    Some(contents_dir.join("Resources"))
}

/// 当前进程所在 .app 的 `Contents/Resources`;
/// 不在 bundle 中(开发期直接跑 `target/**` 下的二进制)时为 `None`。
pub fn bundle_resources_dir() -> Option<PathBuf> {
    bundle_resources_dir_for_exe(&std::env::current_exe().ok()?)
}

fn name_eq(name: Option<&std::ffi::OsStr>, expected: &str) -> bool {
    name.is_some_and(|name| name == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_layout_is_mac_convention() {
        assert_eq!(
            default_app_data_dir_for_home(Path::new("/Users/x")),
            PathBuf::from("/Users/x/Library/Application Support/HearthAdvisor")
        );
    }

    // 环境变量优先级不做进程级测试(避免与并行测试的 env 竞争),
    // 由 app_data_dir 的实现顺序保证:HS_DATA_DIR > HOME > cwd 回退。

    #[test]
    fn bundle_exe_maps_to_sibling_resources() {
        assert_eq!(
            bundle_resources_dir_for_exe(Path::new(
                "/Applications/HearthAdvisor.app/Contents/MacOS/desktop"
            )),
            Some(PathBuf::from(
                "/Applications/HearthAdvisor.app/Contents/Resources"
            ))
        );
    }

    #[test]
    fn dev_binary_is_not_a_bundle() {
        // `cargo run -p desktop`:退回 CWD 下的 rules/,与 bundle 化前一致。
        assert_eq!(
            bundle_resources_dir_for_exe(Path::new("/repo/target/debug/desktop")),
            None
        );
    }

    #[test]
    fn example_binary_is_not_a_bundle() {
        // headless 自查 pools_selfcheck 与主程序走同一套定位,必须同样落回 CWD。
        assert_eq!(
            bundle_resources_dir_for_exe(Path::new("/repo/target/debug/examples/pools_selfcheck")),
            None
        );
    }

    #[test]
    fn missing_contents_level_is_rejected() {
        assert_eq!(
            bundle_resources_dir_for_exe(Path::new("/x/MacOS/desktop")),
            None
        );
    }

    #[test]
    fn non_app_outer_dir_is_rejected() {
        // 目录名对但外层不是 .app —— 误判它会让程序去找不存在的资源目录。
        assert_eq!(
            bundle_resources_dir_for_exe(Path::new("/x/Contents/MacOS/desktop")),
            None
        );
    }
}
