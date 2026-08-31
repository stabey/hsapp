//! 把 Power.log 解析为 Packet NDJSON（每行一条），供 python-hslog 差分对照。
//!
//! ```text
//! cargo run -p hs-log-core --example dump_packets -- <power.log>
//! ```

use hs_log_core::PowerLogParser;
use hs_log_record::{ParseStatus, RawLogRecord};
use hs_types::{LogFileId, LogSessionId};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;
use std::sync::Arc;

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let path = match parse_path() {
        Ok(path) => path,
        Err(usage) => {
            eprintln!("{usage}");
            process::exit(2);
        }
    };
    let text =
        fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let records = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .enumerate()
        .map(|(idx, line)| RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: idx as u64,
            timestamp: None,
            raw_line: Arc::from(line),
            parse_status: ParseStatus::Pending,
        });
    let packets = PowerLogParser::parse_records(records);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    for packet in &packets {
        serde_json::to_writer(&mut out, packet)
            .map_err(|err| format!("serialize packet: {err}"))?;
        out.write_all(b"\n")
            .map_err(|err| format!("write stdout: {err}"))?;
    }
    out.flush().map_err(|err| format!("flush stdout: {err}"))?;
    Ok(())
}

fn parse_path() -> Result<PathBuf, &'static str> {
    let mut args = env::args().skip(1);
    let Some(first) = args.next() else {
        return Err("usage: cargo run -p hs-log-core --example dump_packets -- <power.log>");
    };
    if first == "-h" || first == "--help" {
        return Err("usage: cargo run -p hs-log-core --example dump_packets -- <power.log>");
    }
    if args.next().is_some() {
        return Err("usage: cargo run -p hs-log-core --example dump_packets -- <power.log>");
    }
    Ok(PathBuf::from(first))
}
