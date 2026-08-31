//! `hs-card-sync` — 卡牌数据导入 CLI（M0 Track B）。
//!
//!   cargo run -p hs-card-sync -- import --input <cards.json> --build <id> --locale zhCN

use anyhow::Result;
use hs_card_sync::{legacy_db_hint, parse_args, run_fetch, run_import, CliAction, USAGE};

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    match parse_args(std::env::args())? {
        CliAction::Help => {
            print!("{USAGE}");
            Ok(())
        }
        CliAction::Import(args) => {
            if let Some(hint) = legacy_db_hint(&args) {
                eprintln!("{hint}");
            }
            let report = run_import(&args)?;
            println!(
                "imported {} cards ({} localizations) into build {} (sha256={})",
                report.stats.cards_upserted,
                report.stats.localizations_upserted,
                report.build_id.0,
                report.source_hash
            );
            if let Some(name) = report.sample_name {
                println!("verified CS2_029 {} = {name}", args.locale);
            }
            Ok(())
        }
        CliAction::Fetch(args) => {
            let report = run_fetch(&args)?;
            println!(
                "fetched build {} from {} (sha256={})",
                report.resolved_build.0, report.source_url, report.import.source_hash
            );
            println!(
                "imported {} cards ({} localizations)",
                report.import.stats.cards_upserted, report.import.stats.localizations_upserted
            );
            if let Some(name) = report.import.sample_name {
                println!("verified CS2_029 {} = {name}", args.locale);
            }
            Ok(())
        }
    }
}
