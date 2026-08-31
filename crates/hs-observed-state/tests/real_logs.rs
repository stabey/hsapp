//! 真实对局日志的重放验收（M1 核心验收条款）。
//!
//! fixture 来源见 `tests/logs/real/README.md`（HearthSim 公开测试数据）。
//! 验收对照 PRD：一局真实日志全程不 panic、未识别行保留并计数、
//! 重放任意次数最终状态（hash）相同、friendly_player 可推断。

use hs_log_core::{DomainEvent, EventNormalizer, PowerEventNormalizer, PowerLogParser};
use hs_log_tailer::{ParseStatus, RawLogRecord};
use hs_observed_state::{reduce, state_hash, ObservedGameState};
use hs_types::{GamePhase, LogFileId, LogSessionId, PlayerId};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

fn real_log_records(name: &str) -> Vec<RawLogRecord> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../tests/logs/real/{name}"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.is_empty())
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

/// 完整跑一遍管道：parse → normalize → reduce，返回事件与最终状态。
fn replay(name: &str) -> (Vec<DomainEvent>, ObservedGameState) {
    let packets = PowerLogParser::parse_records(real_log_records(name));
    let mut normalizer = PowerEventNormalizer::new();
    let mut events = Vec::new();
    for packet in &packets {
        events.extend(normalizer.normalize(packet).expect("normalize"));
    }
    let mut state = ObservedGameState::default();
    for event in &events {
        state = reduce(&state, event).expect("reduce");
    }
    (events, state)
}

#[test]
fn full_real_game_replays_without_crash_and_deterministically() {
    let (events_a, state_a) = replay("186852_hearthstone_access_eoe.power.log");
    let (events_b, state_b) = replay("186852_hearthstone_access_eoe.power.log");

    // 重放任意次数结果相同（事件序列 + 最终状态 hash）。
    assert_eq!(events_a, events_b);
    assert_eq!(state_a, state_b);
    assert_eq!(state_hash(&state_a), state_hash(&state_b));

    // 一局打满到分出胜负。
    assert_eq!(state_a.phase, GamePhase::Ended);
    assert!(state_a.winner.is_some());
    assert!(state_a.turn > 1, "turn = {}", state_a.turn);
    assert!(state_a.players.len() >= 2);
    assert!(state_a.entities.len() > 50, "{}", state_a.entities.len());

    // 未识别行保留并计数（META_DATA / SUB_SPELL / DebugPrintGame 等）。
    let warning_events = events_a
        .iter()
        .filter(|e| matches!(e, DomainEvent::ProtocolWarning { .. }))
        .count() as u64;
    assert!(warning_events > 0);
    // warning_count 还包含 Reducer 自身的告警（如 choices id mismatch），只多不少。
    assert!(state_a.warning_count >= warning_events);

    // friendly 推断在真实对局上生效（第一个 ZONE=HAND 的 SHOW_ENTITY）。
    assert!(state_a.friendly_player.is_some());
}

#[test]
fn access_log_mulligan_player_alias_still_resolves_the_unique_choice() {
    let (events, _) = replay("186852_hearthstone_access_eoe.power.log");
    let mut state = ObservedGameState::default();
    let mut saw_placeholder = false;
    let mut saw_resolved_alias = false;

    for event in &events {
        let warning_count = state.warning_count;
        state = reduce(&state, event).expect("reduce");
        match event {
            DomainEvent::ChoicesUpdated {
                id: 1, player_raw, ..
            } if player_raw == "UNKNOWN HUMAN PLAYER" => {
                saw_placeholder = true;
                assert!(state
                    .choices
                    .iter()
                    .any(|choice| choice.id == 1 && choice.player_raw == "UNKNOWN HUMAN PLAYER"));
            }
            DomainEvent::ChoicesResolved {
                id: 1, player_raw, ..
            } if player_raw == "PlayerA#0001" => {
                saw_resolved_alias = true;
                assert!(saw_placeholder);
                assert!(!state.choices.iter().any(|choice| choice.id == 1));
                assert_eq!(state.warning_count, warning_count);
            }
            _ => {}
        }
    }

    assert!(saw_placeholder);
    assert!(saw_resolved_alias);
}

#[test]
fn friendly_player_matches_hslog_ground_truth() {
    // hslog test_friendly_player_superfriends_brawl 的期望：friendly = 1。
    let (_, state) = replay("friendly_player_id_is_1.power.log");
    assert_eq!(state.friendly_player, Some(PlayerId(1)));
}

#[test]
fn friendly_mulligans_coexist_until_each_is_consumed() {
    let (events, _) = replay("friendly_player_id_is_1.power.log");
    let mut state = ObservedGameState::default();
    let mut both_pending = None;
    let mut player_one_sent = false;
    let mut player_two_resolved = false;

    for event in &events {
        let before_ids = state
            .choices
            .iter()
            .filter(|choice| choice.choice_type == "MULLIGAN")
            .map(|choice| choice.id)
            .collect::<BTreeSet<_>>();
        state = reduce(&state, event).expect("reduce");
        let mulligans = state
            .choices
            .iter()
            .filter(|choice| choice.choice_type == "MULLIGAN")
            .collect::<Vec<_>>();
        if mulligans.len() == 2 && both_pending.is_none() {
            both_pending = Some(
                mulligans
                    .iter()
                    .map(|choice| {
                        (
                            choice.id,
                            choice.task_list,
                            choice.player_raw.clone(),
                            choice.issued_revision,
                        )
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let after_ids = mulligans
            .iter()
            .map(|choice| choice.id)
            .collect::<BTreeSet<_>>();
        if both_pending.is_some() {
            match event {
                DomainEvent::ChoicesSent { id: 1, .. } if before_ids.contains(&1) => {
                    player_one_sent = true;
                    assert!(!after_ids.contains(&1));
                    if !player_two_resolved {
                        assert!(after_ids.contains(&2), "player two must remain pending");
                    }
                }
                DomainEvent::ChoicesResolved { id: 2, .. } if before_ids.contains(&2) => {
                    player_two_resolved = true;
                    assert!(!after_ids.contains(&2));
                    assert!(after_ids.contains(&1), "player one must remain pending");
                }
                _ => {}
            }
        }
    }

    let pending = both_pending.expect("both players' mulligans should be pending together");
    assert_eq!(
        pending
            .iter()
            .map(|(id, task_list, player_raw, _)| (*id, *task_list, player_raw.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, Some(6), "PlayerOne"), (2, Some(7), "PlayerTwo"),]
    );
    assert!(pending[0].3 < pending[1].3);
    assert!(
        player_one_sent,
        "player one must be consumed by ChoicesSent"
    );
    assert!(
        player_two_resolved,
        "player two must be consumed by ChoicesResolved"
    );
}

/// 已脱敏的 2026-08-21 标准天梯 fixture。
#[test]
fn local_ranked_standard_loss_replays() {
    let (events_a, state_a) = replay("2026-08-21-ranked-standard-loss.power.log");
    let (events_b, state_b) = replay("2026-08-21-ranked-standard-loss.power.log");
    assert_eq!(events_a, events_b);
    assert_eq!(state_hash(&state_a), state_hash(&state_b));
    assert_eq!(state_a.phase, GamePhase::Ended);
    assert!(state_a.turn > 1, "turn = {}", state_a.turn);
    assert!(state_a.entities.len() > 50, "{}", state_a.entities.len());
    // 证据链见 tests/logs/real/README.md：
    // PlayerA#0001 = PlayerID 1 = 胜者;
    // PlayerB#0002 = PlayerID 2 = friendly_player(负)。
    assert_eq!(state_a.winner, Some(PlayerId(1)));
    assert_eq!(state_a.friendly_player, Some(PlayerId(2)));
}

#[test]
fn local_ranked_reused_choice_id_keeps_each_task_list_identity() {
    let (events, _) = replay("2026-08-21-ranked-standard-loss.power.log");
    let mut state = ObservedGameState::default();
    let mut observations = Vec::new();

    for event in &events {
        state = reduce(&state, event).expect("reduce");
        if let DomainEvent::ChoicesUpdated {
            id: 5, task_list, ..
        } = event
        {
            let just_issued = state
                .choices
                .iter()
                .find(|choice| choice.issued_revision == state.revision)
                .expect("new choice should retain the issuing revision");
            observations.push((
                *task_list,
                just_issued.task_list,
                just_issued.issued_revision,
                state.choices.iter().filter(|choice| choice.id == 5).count(),
            ));
        }
    }

    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].0, Some(827));
    assert_eq!(observations[0].1, Some(827));
    assert_eq!(observations[0].3, 1);
    assert_eq!(observations[1].0, Some(859));
    assert_eq!(observations[1].1, Some(859));
    assert_eq!(observations[1].3, 1);
    assert!(observations[0].2 < observations[1].2);
}
