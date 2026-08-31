//! `FileTailer`：单个 Power.log 路径的最小 tail 实现（M0 TC-01）。

use crate::{LogTail, ParseStatus, RawLogRecord, TailError};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use hs_types::{LogFileId, LogSessionId};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 对单个日志文件做追加读取。文件尚未出现时 `poll` 返回空向量。
#[derive(Debug)]
pub struct FileTailer {
    path: PathBuf,
    session_id: LogSessionId,
    file_id: LogFileId,
    /// 已从文件读到的字节位置（含尚未成行的 leftover）。
    read_pos: u64,
    leftover: Vec<u8>,
}

impl FileTailer {
    /// 从偏移 0 开始跟踪 `path`。会话/文件 ID 默认为 0。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::resume(path, LogSessionId(0), LogFileId(0), 0)
    }

    /// 从已提交偏移恢复（崩溃后按行边界续读）。
    pub fn resume(
        path: impl Into<PathBuf>,
        session_id: LogSessionId,
        file_id: LogFileId,
        committed_offset: u64,
    ) -> Self {
        Self {
            path: path.into(),
            session_id,
            file_id,
            read_pos: committed_offset,
            leftover: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn reset(&mut self) {
        self.read_pos = 0;
        self.leftover.clear();
    }

    fn drain_complete_lines(&mut self) -> Vec<RawLogRecord> {
        let mut records = Vec::new();
        while let Some(nl) = self.leftover.iter().position(|&b| b == b'\n') {
            let line_start = self.read_pos - self.leftover.len() as u64;
            let mut line_bytes: Vec<u8> = self.leftover.drain(..=nl).collect();
            line_bytes.pop(); // '\n'
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let raw_line = String::from_utf8_lossy(&line_bytes);
            records.push(RawLogRecord {
                session_id: self.session_id,
                file_id: self.file_id,
                byte_offset: line_start,
                timestamp: parse_line_timestamp(&raw_line),
                raw_line: Arc::<str>::from(raw_line.as_ref()),
                parse_status: ParseStatus::Pending,
            });
        }
        records
    }
}

impl LogTail for FileTailer {
    fn poll(&mut self) -> Result<Vec<RawLogRecord>, TailError> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(TailError::PermissionDenied(self.path.clone()));
            }
            Err(err) => return Err(err.into()),
        };

        let len = metadata.len();
        let committed = self.committed_offset();
        if committed > len {
            let err = TailError::Truncated {
                offset: committed,
                len,
            };
            self.reset();
            return Err(err);
        }

        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(TailError::PermissionDenied(self.path.clone()));
            }
            Err(err) => return Err(err.into()),
        };

        file.seek(SeekFrom::Start(self.read_pos))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        self.read_pos += buf.len() as u64;
        self.leftover.extend_from_slice(&buf);
        Ok(self.drain_complete_lines())
    }

    fn committed_offset(&self) -> u64 {
        self.read_pos - self.leftover.len() as u64
    }
}

/// 解析 `D HH:MM:SS.frac ...` 行首时间。日志无日期，固定落到 1970-01-01 UTC。
fn parse_line_timestamp(line: &str) -> Option<DateTime<Utc>> {
    let mut parts = line.splitn(3, ' ');
    let _level = parts.next()?;
    let time_token = parts.next()?;
    let time = NaiveTime::parse_from_str(time_token, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(time_token, "%H:%M:%S"))
        .ok()?;
    let date = NaiveDate::from_ymd_opt(1970, 1, 1)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(
        date.and_time(time),
        Utc,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{Seek, Write};

    fn write_all(path: &Path, data: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
    }

    #[test]
    fn waits_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Power.log");
        let mut tail = FileTailer::new(&path);
        assert!(tail.poll().unwrap().is_empty());
        assert_eq!(tail.committed_offset(), 0);
    }

    #[test]
    fn append_writes_are_read_in_batches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Power.log");
        let mut tail = FileTailer::new(&path);

        write_all(&path, b"D 01:00:00.0 first\n");
        let batch1 = tail.poll().unwrap();
        assert_eq!(batch1.len(), 1);
        assert_eq!(batch1[0].byte_offset, 0);
        assert_eq!(batch1[0].raw_line.as_ref(), "D 01:00:00.0 first");
        assert!(batch1[0].timestamp.is_some());
        let after_first = tail.committed_offset();
        assert_eq!(after_first, b"D 01:00:00.0 first\n".len() as u64);

        write_all(&path, b"D 01:00:01.0 second\nD 01:00:02.0 third\n");
        let batch2 = tail.poll().unwrap();
        assert_eq!(batch2.len(), 2);
        assert_eq!(batch2[0].byte_offset, after_first);
        assert_eq!(batch2[0].raw_line.as_ref(), "D 01:00:01.0 second");
        assert_eq!(
            batch2[1].byte_offset,
            after_first + b"D 01:00:01.0 second\n".len() as u64
        );
        assert_eq!(batch2[1].raw_line.as_ref(), "D 01:00:02.0 third");
        assert!(tail.poll().unwrap().is_empty());
    }

    #[test]
    fn half_line_is_buffered_until_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Power.log");
        let mut tail = FileTailer::new(&path);

        write_all(&path, b"D 02:00:00.0 hello");
        assert!(tail.poll().unwrap().is_empty());
        assert_eq!(tail.committed_offset(), 0);

        write_all(&path, b" world\n");
        let records = tail.poll().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].byte_offset, 0);
        assert_eq!(records[0].raw_line.as_ref(), "D 02:00:00.0 hello world");
        assert_eq!(
            tail.committed_offset(),
            b"D 02:00:00.0 hello world\n".len() as u64
        );
    }

    #[test]
    fn truncate_is_detected_then_reread_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Power.log");
        let mut tail = FileTailer::new(&path);

        write_all(&path, b"aaaa\nbbbb\n");
        let first = tail.poll().unwrap();
        assert_eq!(first.len(), 2);
        let offset = tail.committed_offset();
        assert_eq!(offset, 10);

        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"c\n").unwrap();
        file.flush().unwrap();

        let err = tail.poll().unwrap_err();
        match err {
            TailError::Truncated {
                offset: got_off,
                len,
            } => {
                assert_eq!(got_off, offset);
                assert_eq!(len, 2);
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
        assert_eq!(tail.committed_offset(), 0);

        let again = tail.poll().unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].raw_line.as_ref(), "c");
        assert_eq!(again[0].byte_offset, 0);
    }

    #[test]
    fn strips_crlf_and_resumes_from_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Power.log");
        write_all(&path, b"one\r\ntwo\r\n");

        let mut tail = FileTailer::resume(&path, LogSessionId(7), LogFileId(3), 5);
        let records = tail.poll().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, LogSessionId(7));
        assert_eq!(records[0].file_id, LogFileId(3));
        assert_eq!(records[0].raw_line.as_ref(), "two");
        assert_eq!(records[0].byte_offset, 5);
    }
}
