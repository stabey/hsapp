//! Golden Test：`tests/logs/999999-game-open.powerlog.txt` 的逐行归属。

use crate::normalizer::PowerEventNormalizer;
use crate::packet::Packet;
use crate::parser::PowerLogParser;
use crate::{DomainEvent, EventNormalizer, OptionAvailability};
use hs_log_record::{ParseStatus, RawLogRecord};
use hs_types::{EntityId, LogFileId, LogSessionId, PlayerId};
use std::sync::Arc;

pub(crate) fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/logs/999999-game-open.powerlog.txt")
}

pub(crate) fn load_fixture_records() -> Vec<RawLogRecord> {
    let text = std::fs::read_to_string(fixture_path()).unwrap();
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .enumerate()
        .map(|(idx, line)| RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: idx as u64,
            timestamp: None,
            raw_line: Arc::from(line),
            parse_status: ParseStatus::Pending,
        })
        .collect()
}

#[test]
fn golden_game_open_assigns_every_line() {
    let records = load_fixture_records();
    assert!(
        !records.is_empty(),
        "fixture produced no log lines (comments-only?)"
    );

    let packets = PowerLogParser::parse_records(records.clone());

    let mut attributed = 0usize;
    for packet in &packets {
        let src = packet.source_records();
        assert!(!src.is_empty(), "packet {packet:?} has no source records");
        attributed += src.len();
    }
    assert_eq!(
        attributed,
        records.len(),
        "every fixture log line must belong to exactly one packet"
    );

    // CREATE_GAME / FULL_ENTITY / 2×TAG_CHANGE / BLOCK_START / BLOCK_END /
    // PowerTaskList Unknown / LoadingScreen Unknown / SHOW_ENTITY / Options / Choices
    assert_eq!(packets.len(), 11);

    let Packet::CreateGame(create) = &packets[0] else {
        panic!("packets[0] should be CreateGame, got {:?}", packets[0]);
    };
    let game = create.game_entity.as_ref().expect("GameEntity");
    assert_eq!(game.entity_id, EntityId(1));
    assert_eq!(
        game.tags
            .iter()
            .map(|t| (t.name.as_str(), t.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("TURN", "1"), ("ZONE", "PLAY"), ("ENTITY_ID", "1")]
    );
    assert_eq!(create.players.len(), 2);
    assert_eq!(create.players[0].entity_id, EntityId(2));
    assert_eq!(create.players[0].player_id, PlayerId(1));
    assert_eq!(create.players[0].account_hi, Some(0));
    assert_eq!(create.players[0].account_lo, Some(0));
    assert_eq!(create.players[1].entity_id, EntityId(3));
    assert_eq!(create.players[1].player_id, PlayerId(2));
    assert_eq!(create.records.len(), 13);
    assert!(create.records[0].raw_line.contains("CREATE_GAME"));

    let Packet::FullEntity(full) = &packets[1] else {
        panic!("packets[1] should be FullEntity, got {:?}", packets[1]);
    };
    assert_eq!(full.entity_id, EntityId(4));
    assert_eq!(full.card_id.as_ref().unwrap().as_str(), "HERO_01");
    assert_eq!(
        full.tags
            .iter()
            .map(|t| (t.name.as_str(), t.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("HEALTH", "30"),
            ("ZONE", "PLAY"),
            ("CONTROLLER", "1"),
            ("ENTITY_ID", "4"),
            ("CARDTYPE", "HERO"),
        ]
    );
    assert_eq!(full.records.len(), 6);

    let Packet::TagChange(step) = &packets[2] else {
        panic!("packets[2] should be TagChange, got {:?}", packets[2]);
    };
    assert_eq!(step.entity, EntityId(1));
    assert_eq!(step.entity_raw, "GameEntity");
    assert_eq!(step.tag_name, "STEP");
    assert_eq!(step.value_raw, "BEGIN_MULLIGAN");
    assert_eq!(step.tag, 19);
    assert_eq!(step.value, 4);
    assert!(step
        .record
        .as_ref()
        .unwrap()
        .raw_line
        .contains("TAG_CHANGE Entity=GameEntity"));

    let Packet::TagChange(damage) = &packets[3] else {
        panic!("packets[3] should be TagChange, got {:?}", packets[3]);
    };
    assert_eq!(damage.entity, EntityId(4));
    assert!(damage.entity_raw.contains("id=4"));
    assert_eq!(damage.tag_name, "DAMAGE");
    assert_eq!(damage.value_raw, "0");
    assert_eq!(damage.tag, 44);
    assert_eq!(damage.value, 0);

    let Packet::BlockStart(block) = &packets[4] else {
        panic!("packets[4] should be BlockStart, got {:?}", packets[4]);
    };
    assert_eq!(block.block_type, "TRIGGER");
    assert_eq!(block.source, Some(EntityId(1)));
    assert_eq!(block.source_raw, "GameEntity");
    assert_eq!(block.target, None);
    assert_eq!(
        block.effect_card_id.as_ref().map(|c| c.as_str()),
        Some("System")
    );
    assert_eq!(block.trigger_keyword.as_deref(), Some("TRIGGER_VISUAL"));
    assert_eq!(block.records[0].parse_status, ParseStatus::Parsed);
    let block_src = records
        .iter()
        .find(|r| {
            r.raw_line.contains("GameState.DebugPrintPower()") && r.raw_line.contains("BLOCK_START")
        })
        .unwrap();
    assert_eq!(block.records[0].session_id, block_src.session_id);
    assert_eq!(block.records[0].byte_offset, block_src.byte_offset);

    assert!(matches!(&packets[5], Packet::BlockEnd(_)));

    let Packet::Unknown(task_list) = &packets[6] else {
        panic!(
            "packets[6] should be Unknown PowerTaskList, got {:?}",
            packets[6]
        );
    };
    assert!(
        task_list.record.raw_line.contains("PowerTaskList"),
        "PowerTaskList must stay Unknown: {}",
        task_list.record.raw_line
    );
    assert_eq!(task_list.record.parse_status, ParseStatus::Unrecognized);

    let Packet::Unknown(loading) = &packets[7] else {
        panic!(
            "packets[7] should be Unknown LoadingScreen, got {:?}",
            packets[7]
        );
    };
    assert!(
        loading
            .record
            .raw_line
            .contains("LoadingScreen.OnSceneLoaded()"),
        "Unknown must retain LoadingScreen raw line: {}",
        loading.record.raw_line
    );
    assert_eq!(loading.record.parse_status, ParseStatus::Unrecognized);
    let loading_src = records
        .iter()
        .find(|r| r.raw_line.contains("LoadingScreen.OnSceneLoaded()"))
        .unwrap();
    assert_eq!(
        loading.record.raw_line.as_ref(),
        loading_src.raw_line.as_ref()
    );

    let Packet::ShowEntity(show) = &packets[8] else {
        panic!("packets[8] should be ShowEntity, got {:?}", packets[8]);
    };
    assert_eq!(show.entity_id, Some(EntityId(4)));
    assert!(show.entity_raw.contains("id=4"));
    assert_eq!(show.card_id.as_ref().unwrap().as_str(), "HERO_01");
    assert_eq!(show.tags.len(), 2);
    assert_eq!(show.records.len(), 3);

    let Packet::Options(opts) = &packets[9] else {
        panic!("packets[9] should be Options, got {:?}", packets[9]);
    };
    assert_eq!(opts.id, 1);
    assert_eq!(opts.options.len(), 2);
    assert_eq!(opts.options[0].option_type, "END_TURN");
    assert_eq!(opts.options[0].availability, OptionAvailability::Allowed);
    assert_eq!(opts.options[0].raw_error.as_deref(), Some("INVALID"));
    assert_eq!(opts.options[0].error_param.as_deref(), Some(""));
    assert_eq!(opts.options[1].entity, Some(EntityId(4)));
    assert_eq!(opts.options[1].availability, OptionAvailability::Allowed);
    assert_eq!(opts.options[1].targets, vec![EntityId(3)]);
    assert_eq!(opts.options[1].target_entries.len(), 1);
    assert_eq!(opts.options[1].target_entries[0].index, 0);
    assert_eq!(opts.options[1].target_entries[0].entity, Some(EntityId(3)));
    assert_eq!(
        opts.options[1].target_entries[0].availability,
        OptionAvailability::Allowed
    );
    assert!(opts.options[1].target_entries[0]
        .entity_raw
        .contains("id=3"));
    assert_eq!(opts.records.len(), 4);

    let Packet::Choices(ch) = &packets[10] else {
        panic!("packets[10] should be Choices, got {:?}", packets[10]);
    };
    assert_eq!(ch.id, 1);
    assert_eq!(ch.task_list, Some(4));
    assert_eq!(ch.player_entity, Some(EntityId(2)));
    assert_eq!(ch.player_raw, "2");
    assert_eq!(ch.choice_type, "MULLIGAN");
    assert_eq!(ch.min, 0);
    assert_eq!(ch.max, 3);
    assert_eq!(ch.source, Some(EntityId(1)));
    assert_eq!(ch.source_raw, "GameEntity");
    assert_eq!(ch.entities, vec![EntityId(12), EntityId(13)]);
    assert_eq!(ch.records.len(), 4);
}

#[test]
fn golden_fixture_parse_normalize_twice_is_equal() {
    let run = || {
        let packets = PowerLogParser::parse_records(load_fixture_records());
        let mut normalizer = PowerEventNormalizer::new();
        let mut events = Vec::new();
        for packet in &packets {
            events.extend(normalizer.normalize(packet).unwrap());
        }
        events
    };
    let first = run();
    let second = run();
    assert_eq!(first, second);
    assert!(first
        .iter()
        .any(|event| matches!(event, DomainEvent::GameCreated { .. })));
}
