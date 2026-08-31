//! 构建辅助工具。当前只有一个子命令:把 `desktop` 装配成 macOS `.app` bundle。
//!
//! 为什么需要它:裸二进制 `target/debug/desktop` 不被 macOS 当作"应用"——
//! LaunchServices 不认识它,因此无法按应用名授予 TCC 权限(屏幕录制/辅助功能),
//! Dock 与菜单栏也只显示进程名。`.app` 是解决这些的前提,也是将来分发的第一步。
//!
//! 用法:
//!
//! ```text
//! cargo xtask bundle [--release] [--out <dir>] [--no-sign]
//! ```
//!
//! 产物默认落在 `target/<profile>/bundle/HearthAdvisor.app`。路径固定是有意的:
//! macOS 的权限记账与路径相关,每次打到同一位置可减少重新授权次数。
//!
//! 签名为 ad-hoc 自签,只够本机与开发者自用;Developer ID 签名与公证见
//! `docs/adr/0006-distribution-signing-notarization.md`。
//!
//! 注:本 crate 不被任何产品代码依赖,进 workspace 只为跟随
//! fmt / clippy / test 三道质量门,避免构建脚本悄悄失效。

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// Dock / 菜单栏显示名。
const APP_NAME: &str = "Hearth Advisor";
/// bundle 目录名(不含空格,便于命令行操作)。
const BUNDLE_DIR_NAME: &str = "HearthAdvisor.app";
/// 系统据此记账权限的唯一身份。
const BUNDLE_ID: &str = "dev.stabey.hearthadvisor";
/// desktop 的 cargo 包名与产出的二进制名恰好相同。
const DESKTOP_BIN: &str = "desktop";
/// README 声明支持 macOS 13+。
const MIN_MACOS: &str = "13.0";
/// 可选图标源文件名(缺失则跳过,见 PRD D5)。
const ICON_FILE: &str = "AppIcon.icns";
/// 必须随源码和二进制分发的法律文件。顺序稳定,便于测试和审计。
const LEGAL_FILES: [&str; 2] = ["LICENSE", "THIRD_PARTY_NOTICES.html"];
const COPYRIGHT_NOTICE: &str = "Copyright © 2026 stabey";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bundle") => bundle(&parse_bundle_args(&args[1..])?),
        None | Some("-h") | Some("--help") => {
            print!("{}", usage());
            Ok(())
        }
        Some(other) => {
            print!("{}", usage());
            bail!("未知子命令: {other}");
        }
    }
}

fn usage() -> String {
    format!(
        "\
用法: cargo xtask <子命令>

子命令:
  bundle [--release] [--out <dir>] [--no-sign]
          把 {DESKTOP_BIN} 装配成 {BUNDLE_DIR_NAME}(编译 → 组装 → 自签 → 自检)

  --release   编译发布版(默认调试版)
  --out <dir> 产物输出目录(默认 target/<profile>/bundle)
  --no-sign   跳过 ad-hoc 自签(排查签名问题时用)
"
    )
}

// ---------------------------------------------------------------- 参数解析

#[derive(Debug, Default, Eq, PartialEq)]
struct BundleOptions {
    release: bool,
    out_dir: Option<PathBuf>,
    /// 默认签名;`--no-sign` 关闭。
    no_sign: bool,
}

fn parse_bundle_args(args: &[String]) -> Result<BundleOptions> {
    let mut opts = BundleOptions::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--release" => opts.release = true,
            "--no-sign" => opts.no_sign = true,
            "--out" => {
                let dir = iter.next().context("--out 后面缺少目录参数")?;
                if dir.is_empty() || dir.starts_with('-') {
                    bail!(
                        "--out 后面缺少有效目录参数: {dir:?}; 若目录名以 '-' 开头,请写成 ./<目录名>"
                    );
                }
                opts.out_dir = Some(PathBuf::from(dir));
            }
            other => bail!("bundle 不认识的参数: {other}"),
        }
    }
    Ok(opts)
}

// ---------------------------------------------------------------- 打包主流程

fn bundle(opts: &BundleOptions) -> Result<()> {
    ensure_macos_host()?;

    let root = workspace_root();
    let profile = if opts.release { "release" } else { "debug" };
    let rules_src = root.join("rules");
    let out_dir = resolve_output_dir(&root, profile, opts.out_dir.as_deref())?;
    let app_dir = out_dir.join(BUNDLE_DIR_NAME);

    // 从 Cargo 本次 build 的 compiler-artifact 消息取路径,不能猜 root/target:
    // CARGO_TARGET_DIR / build.target-dir / build.target 都可能改变真实产物位置。
    let binary = build_desktop(&root, opts.release)?;
    if !binary.is_file() {
        bail!("编译成功但找不到二进制: {}", binary.display());
    }

    // 整个删掉重建,不做覆盖复制:否则仓库里删掉一条规则后,包里那份还在,
    // 会出现"仓库跑是对的、包里跑多一条规则"这种极难查的偏差。
    if app_dir.exists() {
        fs::remove_dir_all(&app_dir)
            .with_context(|| format!("清理旧产物失败: {}", app_dir.display()))?;
    }

    let summary = cleanup_on_error(
        &app_dir,
        assemble_bundle(&root, &binary, &app_dir, &rules_src, opts.no_sign),
    )?;

    println!();
    println!("✓ {} 打包完成", APP_NAME);
    println!("  产物   : {}", app_dir.display());
    println!("  规则   : {} 个文件", summary.copied_rules);
    println!(
        "  图标   : {}",
        if summary.has_icon {
            "已带上"
        } else {
            "无(系统通用图标)"
        }
    );
    println!(
        "  签名   : {}",
        if opts.no_sign {
            "已跳过"
        } else {
            "ad-hoc 自签"
        }
    );
    println!();
    println!("  启动: open \"{}\"", app_dir.display());
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct BundleSummary {
    has_icon: bool,
    copied_rules: usize,
}

fn assemble_bundle(
    root: &Path,
    binary: &Path,
    app_dir: &Path,
    rules_src: &Path,
    no_sign: bool,
) -> Result<BundleSummary> {
    let contents = app_dir.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources_dir = contents.join("Resources");
    fs::create_dir_all(&macos_dir)?;
    fs::create_dir_all(&resources_dir)?;

    let dest_bin = macos_dir.join(DESKTOP_BIN);
    fs::copy(binary, &dest_bin).with_context(|| format!("复制二进制到 {}", dest_bin.display()))?;
    set_executable(&dest_bin)?;

    // 图标可选:素材存在才带上,不存在静默跳过(PRD D5)。
    let icon_src = root
        .join("apps")
        .join(DESKTOP_BIN)
        .join("resources")
        .join(ICON_FILE);
    let has_icon = icon_src.is_file();
    if has_icon {
        fs::copy(&icon_src, resources_dir.join(ICON_FILE)).context("复制应用图标")?;
    }

    fs::write(contents.join("Info.plist"), info_plist(has_icon)).context("写 Info.plist")?;

    let rules_dest = resources_dir.join("rules");
    let copied = copy_dir_all(rules_src, &rules_dest)
        .with_context(|| format!("复制规则目录 {}", rules_src.display()))?;

    // 法律文件属于分发物的一部分,必须在签名前复制,这样签名也会保护它们。
    copy_legal_files(root, &resources_dir)?;

    if !no_sign {
        codesign(app_dir)?;
    }

    verify(app_dir, root, rules_src, copied, !no_sign)?;

    Ok(BundleSummary {
        has_icon,
        copied_rules: copied,
    })
}

/// 仓库根 = 本 crate 清单目录的父目录。编译期常量,不受运行时 CWD 影响。
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 位于仓库根的子目录下")
        .to_path_buf()
}

fn ensure_macos_host() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("cargo xtask bundle 目前只支持 macOS; Windows 打包留给后续独立子命令");
    }
    Ok(())
}

/// 把输出目录解析成去过 `.` / `..`、并解析了现有 symlink 前缀的绝对路径。
/// 末尾尚不存在的部分原样接回,这样目录还没创建时也能做可靠的包含关系校验。
fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("读取当前目录失败")?
            .join(path)
    };

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                // 绝对路径到根后继续 `..` 仍停在根,与文件系统语义一致。
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    let mut existing = normalized.clone();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .with_context(|| {
                        format!("找不到可解析的输出目录祖先: {}", normalized.display())
                    })?
                    .to_os_string();
                if !existing.pop() {
                    bail!("找不到可解析的输出目录祖先: {}", normalized.display());
                }
                missing.push(name);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("检查输出目录失败: {}", existing.display()));
            }
        }
    }

    let mut resolved = fs::canonicalize(&existing)
        .with_context(|| format!("解析输出目录失败: {}", existing.display()))?;
    for part in missing.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn resolve_output_dir(root: &Path, profile: &str, requested: Option<&Path>) -> Result<PathBuf> {
    let candidate = requested
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join("target").join(profile).join("bundle"));
    let out_dir = canonicalize_allow_missing(&candidate)?;
    let rules_src = fs::canonicalize(root.join("rules")).context("解析仓库 rules 目录失败")?;
    if out_dir.starts_with(&rules_src) {
        bail!(
            "--out 不能放在规则目录内部,否则复制会递归进入产物自身: {}",
            out_dir.display()
        );
    }
    Ok(out_dir)
}

fn cleanup_on_error<T>(app_dir: &Path, result: Result<T>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => match fs::remove_dir_all(app_dir) {
            Ok(()) => Err(err.context("打包失败,已删除半成品产物")),
            Err(cleanup_err) if cleanup_err.kind() == std::io::ErrorKind::NotFound => {
                Err(err.context("打包失败,未留下半成品产物"))
            }
            Err(cleanup_err) => Err(err.context(format!(
                "打包失败,且清理半成品 {} 也失败: {cleanup_err}",
                app_dir.display()
            ))),
        },
    }
}

fn build_desktop(root: &Path, release: bool) -> Result<PathBuf> {
    // 用 CARGO 环境变量而非字面量,以免绕过 rustup 选定的工具链。
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        DESKTOP_BIN,
        "--message-format=json-render-diagnostics",
    ]);
    if release {
        cmd.arg("--release");
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("启动 cargo build 失败")?;
    let stdout = child.stdout.take().context("读取 cargo build 输出失败")?;
    let mut executable = None;
    let mut parse_error = None;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                parse_error.get_or_insert_with(|| {
                    anyhow::Error::new(err).context("读取 Cargo JSON 输出失败")
                });
                continue;
            }
        };
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(err) => {
                parse_error.get_or_insert_with(|| {
                    anyhow::Error::new(err).context("解析 Cargo compiler-artifact JSON 失败")
                });
                continue;
            }
        };
        render_cargo_diagnostic(&message);
        if let Some(path) = compiler_artifact_executable(&message) {
            executable = Some(path);
        }
    }

    let status = child.wait().context("等待 cargo build 失败")?;
    if !status.success() {
        bail!("cargo build -p {DESKTOP_BIN} 失败({status})");
    }
    if let Some(err) = parse_error {
        return Err(err);
    }
    executable.context("cargo build 成功但没有报告 desktop 可执行产物")
}

fn compiler_artifact_executable(message: &Value) -> Option<PathBuf> {
    if message.get("reason")?.as_str()? != "compiler-artifact" {
        return None;
    }
    let target = message.get("target")?;
    if target.get("name")?.as_str()? != DESKTOP_BIN {
        return None;
    }
    let is_binary = target
        .get("kind")?
        .as_array()?
        .iter()
        .any(|kind| kind.as_str() == Some("bin"));
    if !is_binary {
        return None;
    }
    message.get("executable")?.as_str().map(PathBuf::from)
}

fn render_cargo_diagnostic(message: &Value) {
    if message.get("reason").and_then(Value::as_str) != Some("compiler-message") {
        return;
    }
    if let Some(rendered) = message
        .get("message")
        .and_then(|message| message.get("rendered"))
        .and_then(Value::as_str)
    {
        eprint!("{rendered}");
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("设置可执行位: {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    bail!("设置 macOS .app 可执行位需要 Unix 文件权限支持")
}

#[cfg(unix)]
fn has_executable_bit(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn has_executable_bit(_meta: &fs::Metadata) -> bool {
    false
}

fn codesign(app_dir: &Path) -> Result<()> {
    // 不加 --deep:包内没有嵌套代码(otool -L 确认 GPUI 静态链接、无非系统动态库),
    // 且 Apple 已将该参数标记为弃用。
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(app_dir)
        .status()
        .context("启动 codesign 失败")?;
    if !status.success() {
        bail!("codesign 自签失败({status})");
    }
    Ok(())
}

// ---------------------------------------------------------------- Info.plist

fn info_plist(has_icon: bool) -> String {
    // 版本号来自 workspace(xtask 与 desktop 都写 version.workspace = true),
    // 该假设由 desktop_version_is_inherited_from_workspace 测试钉住。
    let version = env!("CARGO_PKG_VERSION");
    let icon_entry = if has_icon {
        format!("    <key>CFBundleIconFile</key>\n    <string>{ICON_FILE}</string>\n")
    } else {
        String::new()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>{BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>
    <string>{DESKTOP_BIN}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>{MIN_MACOS}</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>NSHumanReadableCopyright</key>
    <string>{COPYRIGHT_NOTICE}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
{icon_entry}</dict>
</plist>
"#
    )
}

// ---------------------------------------------------------------- 目录复制

/// 递归复制目录,返回复制的文件数(用于自检对账)。
fn copy_dir_all(src: &Path, dst: &Path) -> Result<usize> {
    fs::create_dir_all(dst)?;
    let mut count = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            count += copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).with_context(|| format!("复制 {}", from.display()))?;
            count += 1;
        }
    }
    Ok(count)
}

fn count_files(dir: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += count_files(&entry.path())?;
        } else {
            count += 1;
        }
    }
    Ok(count)
}

fn copy_legal_files(root: &Path, resources_dir: &Path) -> Result<()> {
    let mut files = Vec::with_capacity(LEGAL_FILES.len());
    for name in LEGAL_FILES {
        let source = root.join(name);
        let bytes =
            fs::read(&source).with_context(|| format!("读取法律文件失败: {}", source.display()))?;
        if bytes.is_empty() {
            bail!("法律文件不能为空: {}", source.display());
        }
        files.push((name, bytes));
    }

    let legal_dir = resources_dir.join("Legal");
    fs::create_dir_all(&legal_dir)
        .with_context(|| format!("创建法律文件目录失败: {}", legal_dir.display()))?;
    for (name, bytes) in files {
        let destination = legal_dir.join(name);
        fs::write(&destination, bytes)
            .with_context(|| format!("复制法律文件失败: {}", destination.display()))?;
    }
    Ok(())
}

fn verify_legal_files(root: &Path, app_dir: &Path) -> Result<()> {
    let legal_dir = app_dir.join("Contents").join("Resources").join("Legal");
    for name in LEGAL_FILES {
        let source = root.join(name);
        let destination = legal_dir.join(name);
        let expected =
            fs::read(&source).with_context(|| format!("读取法律文件失败: {}", source.display()))?;
        let actual = fs::read(&destination)
            .with_context(|| format!("包内法律文件缺失: {}", destination.display()))?;
        if expected.is_empty() {
            bail!("法律文件不能为空: {}", source.display());
        }
        if expected != actual {
            bail!("包内法律文件与仓库不一致: {}", destination.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 自检

/// 产物自检。CI 的"日常体检"就是靠跑一次 bundle 让这里把关。
fn verify(
    app_dir: &Path,
    root: &Path,
    rules_src: &Path,
    copied: usize,
    signed: bool,
) -> Result<()> {
    let exe = app_dir.join("Contents").join("MacOS").join(DESKTOP_BIN);
    let meta = fs::metadata(&exe).with_context(|| format!("缺可执行文件: {}", exe.display()))?;
    if !has_executable_bit(&meta) {
        bail!("{} 没有可执行位", exe.display());
    }

    let plist = app_dir.join("Contents").join("Info.plist");
    let status = Command::new("plutil")
        .arg("-lint")
        .arg(&plist)
        .status()
        .context("启动 plutil 失败")?;
    if !status.success() {
        bail!("Info.plist 语法校验未通过: {}", plist.display());
    }

    let rules_dest = app_dir.join("Contents").join("Resources").join("rules");
    for sub in ["cards", "pools"] {
        let dir = rules_dest.join(sub);
        if !dir.is_dir() {
            bail!("规则子目录缺失: {}", dir.display());
        }
    }
    let expected = count_files(rules_src)?;
    let actual = count_files(&rules_dest)?;
    if expected != actual || expected != copied {
        bail!("规则文件数对不上: 源 {expected} / 包内 {actual} / 复制 {copied}");
    }

    verify_legal_files(root, app_dir)?;

    if signed {
        let status = Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(app_dir)
            .status()
            .context("启动 codesign --verify 失败")?;
        if !status.success() {
            bail!("签名校验未通过({status})");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_case(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hsapp-xtask-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn opts(args: &[&str]) -> Result<BundleOptions> {
        parse_bundle_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn default_options_are_debug_and_signed() {
        assert_eq!(opts(&[]).unwrap(), BundleOptions::default());
    }

    #[test]
    fn flags_parse_in_any_order() {
        let parsed = opts(&["--no-sign", "--out", "/tmp/x", "--release"]).unwrap();
        assert_eq!(
            parsed,
            BundleOptions {
                release: true,
                out_dir: Some(PathBuf::from("/tmp/x")),
                no_sign: true,
            }
        );
    }

    #[test]
    fn unknown_flag_is_rejected() {
        // 打错开关时必须报错,不能被当成默认行为悄悄跑一遍。
        assert!(opts(&["--relase"]).is_err());
    }

    #[test]
    fn out_without_value_is_rejected() {
        assert!(opts(&["--out"]).is_err());
    }

    #[test]
    fn out_value_that_looks_like_an_option_is_rejected() {
        assert!(opts(&["--out", "--release"]).is_err());
        assert!(opts(&["--out", "--no-sign"]).is_err());
    }

    #[test]
    fn output_inside_rules_is_rejected_before_copying() {
        let root = workspace_root();
        let requested = root.join("rules").join("bundle-regression-test");
        let err = resolve_output_dir(&root, "debug", Some(&requested)).unwrap_err();
        assert!(
            err.to_string().contains("不能放在规则目录内部"),
            "unexpected error: {err:#}"
        );
        assert!(!requested.exists(), "校验不应创建输出目录");
    }

    #[test]
    fn compiler_artifact_path_comes_from_cargo_message() {
        let message = serde_json::json!({
            "reason": "compiler-artifact",
            "target": { "name": "desktop", "kind": ["bin"] },
            "executable": "/custom/cargo-target/aarch64-apple-darwin/debug/desktop"
        });
        assert_eq!(
            compiler_artifact_executable(&message),
            Some(PathBuf::from(
                "/custom/cargo-target/aarch64-apple-darwin/debug/desktop"
            ))
        );
    }

    #[test]
    fn failed_assembly_removes_partial_bundle() {
        let app_dir = temp_case("cleanup").with_extension("app");
        fs::create_dir_all(app_dir.join("Contents")).unwrap();
        fs::write(app_dir.join("Contents/Info.plist"), "partial").unwrap();

        let result: Result<()> = cleanup_on_error(&app_dir, Err(anyhow::anyhow!("模拟签名失败")));
        assert!(result.is_err());
        assert!(!app_dir.exists(), "失败后不应留下外观完整的 .app");
    }

    #[test]
    fn legal_files_are_copied_byte_for_byte() {
        let root = temp_case("legal-copy-root");
        let resources = temp_case("legal-copy-resources");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("LICENSE"), b"license\n").unwrap();
        fs::write(
            root.join("THIRD_PARTY_NOTICES.html"),
            b"<html>notices</html>\n",
        )
        .unwrap();

        copy_legal_files(&root, &resources).unwrap();

        for name in LEGAL_FILES {
            assert_eq!(
                fs::read(root.join(name)).unwrap(),
                fs::read(resources.join("Legal").join(name)).unwrap()
            );
        }

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(resources).unwrap();
    }

    #[test]
    fn missing_legal_file_removes_partial_bundle() {
        let root = temp_case("legal-missing-root");
        let app_dir = temp_case("legal-missing").with_extension("app");
        let resources = app_dir.join("Contents/Resources");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&resources).unwrap();
        fs::write(root.join("LICENSE"), b"license\n").unwrap();

        let result = cleanup_on_error(&app_dir, copy_legal_files(&root, &resources));
        assert!(result.is_err());
        assert!(!app_dir.exists(), "法律文件缺失后不应留下半成品 .app");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plist_carries_identity_keys() {
        let plist = info_plist(false);
        for needle in [
            "<string>Hearth Advisor</string>",
            "<string>dev.stabey.hearthadvisor</string>",
            "<string>desktop</string>",
            "<string>APPL</string>",
            "<key>NSHighResolutionCapable</key>",
            "<string>13.0</string>",
            "<key>NSHumanReadableCopyright</key>",
            "<string>Copyright © 2026 stabey</string>",
        ] {
            assert!(plist.contains(needle), "Info.plist 缺 {needle}:\n{plist}");
        }
    }

    #[test]
    fn plist_omits_icon_key_when_no_icon() {
        // 写了 CFBundleIconFile 却没有对应文件,Finder 会显示破损图标。
        assert!(!info_plist(false).contains("CFBundleIconFile"));
        assert!(info_plist(true).contains("AppIcon.icns"));
    }

    #[test]
    fn desktop_version_is_inherited_from_workspace() {
        // Info.plist 的版本号用 xtask 自己的 CARGO_PKG_VERSION,前提是
        // desktop 也继承 workspace 版本。谁给 desktop 单独设版本号,这条会失败,
        // 提示去改 info_plist 的版本来源,而不是让包里悄悄写错版本。
        let manifest = fs::read_to_string(workspace_root().join("apps/desktop/Cargo.toml"))
            .expect("读 apps/desktop/Cargo.toml");
        assert!(
            manifest.contains("version.workspace = true"),
            "desktop 不再继承 workspace 版本,info_plist 的版本来源需同步修改"
        );
    }
}
