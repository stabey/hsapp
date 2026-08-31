//! Neutral raw-log record contract shared by tailing and parsing layers.
//!
//! This leaf crate owns only the serializable input record. It deliberately
//! contains no file IO, parser, database, or UI dependency.

use chrono::{DateTime, Utc};
use hs_types::{LogFileId, LogSessionId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One source log line. Raw input is never discarded (ADR-015).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawLogRecord {
    pub session_id: LogSessionId,
    pub file_id: LogFileId,
    /// Byte offset of the beginning of this line, used for resume and dedupe.
    pub byte_offset: u64,
    /// Timestamp parsed from the line prefix, when present.
    pub timestamp: Option<DateTime<Utc>>,
    pub raw_line: Arc<str>,
    pub parse_status: ParseStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ParseStatus {
    /// Not yet passed to a parser.
    Pending,
    /// Recognized as a packet.
    Parsed,
    /// Not recognized; the raw line is retained as an unknown packet.
    Unrecognized,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn raw_log_record_round_trips_through_json() {
        let record = RawLogRecord {
            session_id: LogSessionId(17),
            file_id: LogFileId(4),
            byte_offset: 1234,
            timestamp: Some(Utc.with_ymd_and_hms(2026, 8, 30, 9, 45, 12).unwrap()),
            raw_line: Arc::from("D 09:45:12.0000000 GameState.DebugPrintPower() - CREATE_GAME"),
            parse_status: ParseStatus::Parsed,
        };

        let json = serde_json::to_string(&record).expect("serialize raw log record");
        let decoded: RawLogRecord =
            serde_json::from_str(&json).expect("deserialize raw log record");

        assert_eq!(decoded, record);
    }

    #[test]
    fn every_parse_status_round_trips() {
        for status in [
            ParseStatus::Pending,
            ParseStatus::Parsed,
            ParseStatus::Unrecognized,
        ] {
            let json = serde_json::to_string(&status).expect("serialize parse status");
            let decoded: ParseStatus =
                serde_json::from_str(&json).expect("deserialize parse status");
            assert_eq!(decoded, status);
        }
    }
}
