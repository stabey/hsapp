//! 对局原始 Power.log 段存档(PRD 决策 4)。
//!
//! 输入日志文本 + [`GameId`],写 `data/games/<id>.power.log`,返回路径与 sha256。
//! 失败返回错误,由调用方决定是否降级(统计入库不受本函数强制绑定)。

use crate::DecksError;
use hs_types::GameId;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// 一次成功落盘的存档描述。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogArchive {
    pub path: PathBuf,
    /// 64 字符小写 hex。
    pub sha256: String,
}

/// 把该局 Power.log 段写入 `{data_dir}/games/{game_id}.power.log`。
///
/// 先写同目录临时文件再 rename,避免半截文件被当成完成存档。
pub fn archive_power_log(
    data_dir: impl AsRef<Path>,
    game_id: GameId,
    log_text: &str,
) -> Result<LogArchive, DecksError> {
    let games_dir = data_dir.as_ref().join("games");
    fs::create_dir_all(&games_dir)?;

    let file_name = format!("{}.power.log", game_id.0);
    let dest = games_dir.join(&file_name);
    let tmp = games_dir.join(format!(".{file_name}.tmp"));

    let bytes = log_text.as_bytes();
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &dest)?;

    Ok(LogArchive {
        path: dest,
        sha256: hex_encode(&Sha256::digest(bytes)),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("hs-decks-archive-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn writes_path_and_sha256() {
        let dir = scratch_dir();
        let text = "CREATE_GAME\nTAG_CHANGE Entity=GameEntity tag=STATE value=COMPLETE\n";
        let archived = archive_power_log(&dir, GameId(42), text).unwrap();
        assert_eq!(archived.path, dir.join("games").join("42.power.log"));
        assert_eq!(fs::read_to_string(&archived.path).unwrap(), text);
        assert_eq!(
            archived.sha256,
            hex_encode(&Sha256::digest(text.as_bytes()))
        );
        assert_eq!(archived.sha256.len(), 64);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_log_has_known_sha256() {
        let dir = scratch_dir();
        let archived = archive_power_log(&dir, GameId(1), "").unwrap();
        assert_eq!(
            archived.sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
