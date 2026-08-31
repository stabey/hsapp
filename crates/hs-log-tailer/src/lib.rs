//! `hs-log-tailer` — Power.log 文件追踪（M0 Track C：`FileTailer`）。
//!
//! 契约要求（设计文档 §7.2）：文件出现前等待、追加读取、半行缓冲、
//! byte offset 记录、截断检测、崩溃后从偏移恢复、重复读取去重。

mod file_tailer;

pub use file_tailer::FileTailer;
pub use hs_log_record::{ParseStatus, RawLogRecord};

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum TailError {
    #[error("log file not found: {0}")]
    FileNotFound(PathBuf),
    #[error("read permission denied: {0}")]
    PermissionDenied(PathBuf),
    #[error("file truncated (offset {offset} beyond new len {len})")]
    Truncated { offset: u64, len: u64 },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 日志追踪器契约。实现必须可从记录的 offset 恢复。
pub trait LogTail {
    /// 拉取自上次调用以来的新完整行（半行留在内部缓冲）。
    fn poll(&mut self) -> Result<Vec<RawLogRecord>, TailError>;

    /// 当前已消费到的字节偏移（持久化用）。
    fn committed_offset(&self) -> u64;
}

#[cfg(test)]
mod contract_tests {
    use super::{ParseStatus, RawLogRecord};
    use hs_log_record::{ParseStatus as LeafParseStatus, RawLogRecord as LeafRawLogRecord};

    #[test]
    fn record_types_are_leaf_reexports() {
        assert_eq!(
            std::any::TypeId::of::<RawLogRecord>(),
            std::any::TypeId::of::<LeafRawLogRecord>()
        );
        assert_eq!(
            std::any::TypeId::of::<ParseStatus>(),
            std::any::TypeId::of::<LeafParseStatus>()
        );
    }
}
