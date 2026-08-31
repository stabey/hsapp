//! log.config 校验（M0 拆解 D-03 / TC-03）。
//!
//! 缺文件 → `Failed`（没有配置则客户端不会写出 Power.log）。
//! 缺 `[Power]` 或缺/错 `LogLevel=1` / `FilePrinting=true` → `Degraded`，
//! reason 列出缺哪些项。齐全 → `Healthy`。
//!
//! `Verbose` 等常见增强项本轮不强制（报告给主 Agent）。

use crate::PlatformError;
use hs_diagnostics::ComponentHealth;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// M0 要求的 `[Power]` 项（D-03 原文）。
pub const REQUIRED_POWER_KEYS: &[&str] = &["LogLevel=1", "FilePrinting=true"];

/// 一键写入要保证的 `[Power]` 项。比校验必需项多 `Verbose=true`：
/// M1 需要 Options/Choices 等细节行，采集真实日志前统一写入
/// （Verbose 覆盖面复评见 M1 R4；校验仍只强制 REQUIRED_POWER_KEYS）。
pub const ENSURED_POWER_ENTRIES: &[(&str, &str)] = &[
    ("LogLevel", "1"),
    ("FilePrinting", "true"),
    ("Verbose", "true"),
];

/// 一键写入要保证的 `[Decks]` 项（M2）。客户端据此写出 `Decks.log`，
/// 是牌组自动导入的前提；缺这段只影响自动导入，不影响 Power.log 链路。
pub const ENSURED_DECKS_ENTRIES: &[(&str, &str)] = &[("LogLevel", "1"), ("FilePrinting", "true")];

/// 一键写入覆盖的全部段落。
pub const ENSURED_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    ("Power", ENSURED_POWER_ENTRIES),
    ("Decks", ENSURED_DECKS_ENTRIES),
];

/// `ensure_log_config` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfigWriteReport {
    /// 文件内容是否发生了变化（false = 本来就满足，未写）。
    pub changed: bool,
    /// 写入前的备份路径（仅当原文件存在且发生写入）。
    pub backup_path: Option<PathBuf>,
    /// 变更需重启炉石才生效（游戏运行状态无法探测，有变更即提示）。
    pub needs_restart: bool,
}

/// 一键写入 log.config（M1 S1）。
///
/// 合并式：保留既有全部段落与键，只补/改 [`ENSURED_POWER_ENTRIES`]；
/// 内容已满足时不落盘。写入前将原文件备份为
/// `<name>.bak-<unix_ts>`（原文件不存在则直接创建，含父目录）。
pub fn ensure_log_config(path: &Path) -> Result<LogConfigWriteReport, PlatformError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(contents) => Some(contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };

    if let Some(text) = existing.as_deref() {
        if ensured_entries_satisfied(text) {
            return Ok(LogConfigWriteReport {
                changed: false,
                backup_path: None,
                needs_restart: false,
            });
        }
    }
    let mut merged = existing.clone().unwrap_or_default();
    for (section, entries) in ENSURED_SECTIONS {
        merged = merge_section_entries(&merged, section, entries);
    }

    let backup_path = if existing.is_some() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "log.config".to_string());
        name.push_str(&format!(".bak-{ts}"));
        let backup = path.with_file_name(name);
        std::fs::copy(path, &backup)?;
        Some(backup)
    } else {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        None
    };

    std::fs::write(path, merged)?;
    Ok(LogConfigWriteReport {
        changed: true,
        backup_path,
        needs_restart: true,
    })
}

/// 语义判定 [`ENSURED_SECTIONS`] 是否已全部满足（大小写/空白不敏感）。
fn ensured_entries_satisfied(text: &str) -> bool {
    let sections = parse_ini(text);
    ENSURED_SECTIONS.iter().all(|(name, entries)| {
        sections.get(*name).is_some_and(|found| {
            entries.iter().all(|(key, _)| {
                entry_value_ok(key, found.get(&key.to_ascii_lowercase()).map_or("", |v| v))
            })
        })
    })
}

/// 在原文文本上合并某段的必需项，尽量保持原有行序与注释。
fn merge_section_entries(text: &str, section: &str, entries: &[(&str, &str)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_target = false;
    let mut target_seen = false;
    let mut pending: Vec<(&str, &str)> = entries.to_vec();

    let flush_pending = |out: &mut Vec<String>, pending: &mut Vec<(&str, &str)>| {
        for (key, value) in pending.drain(..) {
            out.push(format!("{key}={value}"));
        }
    };

    for raw in text.lines() {
        let line = raw.trim();
        let is_section = line.starts_with('[') && line.ends_with(']');
        if is_section {
            if in_target {
                flush_pending(&mut out, &mut pending);
            }
            in_target = line[1..line.len() - 1].trim().eq_ignore_ascii_case(section);
            if in_target {
                target_seen = true;
            }
            out.push(raw.to_string());
            continue;
        }
        if in_target {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if let Some(pos) = pending
                    .iter()
                    .position(|(k, _)| k.eq_ignore_ascii_case(key))
                {
                    let (k, v) = pending.remove(pos);
                    if entry_value_ok(k, value) {
                        out.push(raw.to_string());
                    } else {
                        out.push(format!("{k}={v}"));
                    }
                    continue;
                }
            }
        }
        out.push(raw.to_string());
    }

    if in_target {
        flush_pending(&mut out, &mut pending);
    } else if !target_seen {
        if !out.is_empty() && !out.last().is_some_and(|l| l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(format!("[{section}]"));
        flush_pending(&mut out, &mut pending);
    }

    let mut merged = out.join("\n");
    merged.push('\n');
    merged
}

/// 读取并校验 `log.config`。文件不存在或不可读为 `Failed`。
pub fn validate_log_config(path: &Path) -> ComponentHealth {
    match std::fs::read_to_string(path) {
        Ok(contents) => validate_log_config_str(&contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ComponentHealth::Failed {
            reason: format!("log.config not found: {}", path.display()),
        },
        Err(err) => ComponentHealth::Failed {
            reason: format!("cannot read log.config {}: {err}", path.display()),
        },
    }
}

/// 校验已经读入的 log.config 文本。
pub fn validate_log_config_str(contents: &str) -> ComponentHealth {
    let sections = parse_ini(contents);
    let Some(power) = sections.get("Power") else {
        return ComponentHealth::Degraded {
            reason: "missing [Power] section".to_string(),
        };
    };

    let mut missing = Vec::new();
    if !log_level_ok(power.get("loglevel").map(String::as_str)) {
        missing.push("LogLevel=1");
    }
    if !truthy(power.get("fileprinting").map(String::as_str)) {
        missing.push("FilePrinting=true");
    }

    if missing.is_empty() {
        ComponentHealth::Healthy
    } else {
        ComponentHealth::Degraded {
            reason: format!("missing or invalid: {}", missing.join(", ")),
        }
    }
}

/// 单个 ensured 键的既有值是否已满足（满足则保留原行）。
fn entry_value_ok(key: &str, value: &str) -> bool {
    if key.eq_ignore_ascii_case("LogLevel") {
        log_level_ok(Some(value))
    } else {
        truthy(Some(value))
    }
}

fn log_level_ok(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1"))
}

fn truthy(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("true" | "1" | "yes")
    )
}

fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim().to_string();
            sections.entry(current.clone()).or_default();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    const COMPLETE: &str = "[Power]\nLogLevel=1\nFilePrinting=true\n";
    const MISSING_PRINTING: &str = "[Power]\nLogLevel=1\n";
    const WRONG_LEVEL: &str = "[Power]\nLogLevel=0\nFilePrinting=True\n";

    #[test]
    fn complete_config_is_healthy() {
        assert!(matches!(
            validate_log_config_str(COMPLETE),
            ComponentHealth::Healthy
        ));
        assert!(matches!(
            validate_log_config_str("[Power]\nLogLevel = 1\nFilePrinting = True\n"),
            ComponentHealth::Healthy
        ));
    }

    #[test]
    fn missing_items_are_degraded_with_names() {
        match validate_log_config_str(MISSING_PRINTING) {
            ComponentHealth::Degraded { reason } => {
                assert!(reason.contains("FilePrinting=true"), "{reason}");
                assert!(!reason.contains("LogLevel=1"), "{reason}");
            }
            other => panic!("expected Degraded, got {other:?}"),
        }
        match validate_log_config_str(WRONG_LEVEL) {
            ComponentHealth::Degraded { reason } => {
                assert!(reason.contains("LogLevel=1"), "{reason}");
            }
            other => panic!("expected Degraded, got {other:?}"),
        }
        match validate_log_config_str("[Zone]\nLogLevel=1\n") {
            ComponentHealth::Degraded { reason } => {
                assert!(reason.contains("missing [Power] section"), "{reason}");
            }
            other => panic!("expected Degraded, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        match validate_log_config(&path) {
            ComponentHealth::Failed { reason } => {
                assert!(reason.contains("not found"), "{reason}");
                assert!(reason.contains("log.config"), "{reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn ensure_creates_missing_file_with_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("Preferences/Blizzard/Hearthstone/log.config");
        let report = ensure_log_config(&path).unwrap();
        assert!(report.changed);
        assert!(report.needs_restart);
        assert_eq!(report.backup_path, None);

        let written = fs::read_to_string(&path).unwrap();
        assert!(matches!(
            validate_log_config_str(&written),
            ComponentHealth::Healthy
        ));
        assert!(ensured_entries_satisfied(&written), "{written}");
    }

    #[test]
    fn ensure_merges_and_backs_up_preserving_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        fs::write(
            &path,
            "[Zone]\nLogLevel=1\nFilePrinting=true\n\n[Power]\nLogLevel=0\nConsolePrinting=false\n",
        )
        .unwrap();

        let report = ensure_log_config(&path).unwrap();
        assert!(report.changed);
        let backup = report.backup_path.expect("backup for existing file");
        assert!(backup.exists());
        assert!(fs::read_to_string(&backup).unwrap().contains("LogLevel=0"));

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[Zone]"), "{written}");
        assert!(written.contains("ConsolePrinting=false"), "{written}");
        assert!(written.contains("LogLevel=1"), "{written}");
        assert!(written.contains("Verbose=true"), "{written}");
        assert!(ensured_entries_satisfied(&written), "{written}");
    }

    #[test]
    fn ensure_writes_decks_section_for_auto_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        fs::write(&path, COMPLETE).unwrap();

        let report = ensure_log_config(&path).unwrap();
        assert!(report.changed, "缺 [Decks] 段必须补写");

        let written = fs::read_to_string(&path).unwrap();
        let sections = parse_ini(&written);
        let decks = sections.get("Decks").expect("[Decks] section");
        assert_eq!(decks.get("loglevel").map(String::as_str), Some("1"));
        assert!(truthy(decks.get("fileprinting").map(String::as_str)));
        // 既有 [Power] 段不受影响
        assert!(ensured_entries_satisfied(&written), "{written}");
    }

    #[test]
    fn ensure_preserves_existing_decks_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        let original = "[Power]\nLogLevel=1\nFilePrinting=true\nVerbose=true\n\
                        \n[Decks]\nLogLevel = 1\nFilePrinting = True\n";
        fs::write(&path, original).unwrap();

        let report = ensure_log_config(&path).unwrap();
        assert!(!report.changed, "两段都已满足时不应重写");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn ensure_is_noop_when_semantically_satisfied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        // 格式随意（空格、大小写）但语义齐全 → 不应重写。
        let original = "[Power]\nLogLevel = 1\nFilePrinting = True\nVerbose = yes\n\
                        \n[Decks]\nLogLevel = 1\nFilePrinting = yes\n";
        fs::write(&path, original).unwrap();

        let report = ensure_log_config(&path).unwrap();
        assert!(!report.changed);
        assert!(!report.needs_restart);
        assert_eq!(report.backup_path, None);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn ensure_appends_power_section_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        fs::write(&path, "[Achievements]\nLogLevel=1\n").unwrap();

        let report = ensure_log_config(&path).unwrap();
        assert!(report.changed);
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[Achievements]"), "{written}");
        assert!(ensured_entries_satisfied(&written), "{written}");
    }

    #[test]
    fn complete_file_fixture_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.config");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(COMPLETE.as_bytes()).unwrap();
        file.flush().unwrap();
        assert!(matches!(
            validate_log_config(&path),
            ComponentHealth::Healthy
        ));
    }
}
