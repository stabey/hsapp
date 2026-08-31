//! 真实日志逐事件 derive:关键节点断言 + 同一日志跑 3 次结果全等。

use hs_log_core::{DomainEvent, EventNormalizer, PowerEventNormalizer, PowerLogParser};
use hs_log_tailer::{ParseStatus, RawLogRecord};
use hs_observed_state::{reduce, ObservedGameState};
use hs_tracker::{derive, TrackerView};
use hs_types::{LogFileId, LogSessionId, PlayerId, Zone};
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

fn events_of(name: &str) -> Vec<DomainEvent> {
    let packets = PowerLogParser::parse_records(real_log_records(name));
    let mut normalizer = PowerEventNormalizer::new();
    let mut events = Vec::new();
    for packet in &packets {
        events.extend(normalizer.normalize(packet).expect("normalize"));
    }
    events
}

fn friendly_zone_count(state: &ObservedGameState, zone: Zone) -> usize {
    let Some(friendly) = state.friendly_player else {
        return 0;
    };
    state
        .entities
        .values()
        .filter(|entity| entity.controller == Some(friendly) && entity.zone == zone)
        .count()
}

fn derive_n_times(name: &str, n: usize) -> Vec<TrackerView> {
    let events = events_of(name);
    (0..n)
        .map(|_| {
            let mut state = ObservedGameState::default();
            for event in &events {
                state = reduce(&state, event).expect("reduce");
            }
            derive(&state, None)
        })
        .collect()
}

const EOE_LOG: &str = "186852_hearthstone_access_eoe.power.log";
const RANKED_LOG: &str = "2026-08-21-ranked-standard-loss.power.log";

#[test]
fn eoe_log_key_nodes_and_draw_decreases() {
    let events = events_of(EOE_LOG);
    let mut state = ObservedGameState::default();
    let mut first_friendly_deck = None;
    let mut prev_deck = None;
    let mut saw_decrease = false;
    let mut first_opponent_play = None;

    for event in &events {
        state = reduce(&state, event).expect("reduce");
        let view = derive(&state, None);
        if state.friendly_player.is_some() && first_friendly_deck.is_none() {
            let deck = friendly_zone_count(&state, Zone::Deck);
            let hand = friendly_zone_count(&state, Zone::Hand);
            first_friendly_deck = Some((deck, hand, deck + hand));
            prev_deck = Some(deck);
        }
        if let Some(prev) = prev_deck {
            let deck = friendly_zone_count(&state, Zone::Deck);
            if deck < prev {
                saw_decrease = true;
            }
            prev_deck = Some(deck);
        }
        if first_opponent_play.is_none() && !view.opponent_played.is_empty() {
            first_opponent_play = Some(view.opponent_played.len());
        }
    }

    let (deck, hand, total) = first_friendly_deck.expect("friendly never detected");
    // 开局:友方牌库+手牌合计 30 张(不含英雄/英雄技能/硬币等生成牌的 DECK 计数)。
    assert_eq!(total, 30, "opening friendly deck+hand = {deck}+{hand}");
    assert!(deck >= 26, "opening remaining deck = {deck}");
    assert!(saw_decrease, "friendly remaining deck never decreased");
    assert!(
        first_opponent_play.unwrap_or(0) >= 1,
        "opponent never played a visible card"
    );

    let final_view = derive(&state, None);
    assert!(final_view.game.friendly_player.is_some());
    assert!(final_view.opponent_played.len() >= 8);
    assert_eq!(final_view.game.phase, hs_types::GamePhase::Ended);
}

#[test]
fn ranked_log_key_nodes() {
    let events = events_of(RANKED_LOG);
    let mut state = ObservedGameState::default();
    let mut first_friendly_total = None;
    let mut saw_decrease = false;
    let mut prev_deck = None;

    for event in &events {
        state = reduce(&state, event).expect("reduce");
        if state.friendly_player.is_some() && first_friendly_total.is_none() {
            let deck = friendly_zone_count(&state, Zone::Deck);
            let hand = friendly_zone_count(&state, Zone::Hand);
            first_friendly_total = Some((deck, hand, deck + hand));
            prev_deck = Some(deck);
        }
        if let Some(prev) = prev_deck {
            let deck = friendly_zone_count(&state, Zone::Deck);
            if deck < prev {
                saw_decrease = true;
            }
            prev_deck = Some(deck);
        }
    }

    let (deck, hand, total) = first_friendly_total.expect("friendly never detected");
    assert_eq!(total, 30, "opening friendly deck+hand = {deck}+{hand}");
    assert!(saw_decrease, "friendly remaining deck never decreased");

    let view = derive(&state, None);
    // Fixture 固定答案:友方=玩家 2(PlayerB),胜者=玩家 1(PlayerA)。
    // 证据来自首个可见手牌信号与终局 PLAYSTATE,见 real/README.md。
    assert_eq!(view.game.friendly_player, Some(PlayerId(2)));
    assert_eq!(view.game.winner, Some(PlayerId(1)));
    assert!(view.opponent_played.len() >= 5);
    assert_eq!(view.game.phase, hs_types::GamePhase::Ended);
}

#[test]
fn same_log_derived_three_times_is_equal() {
    for name in [EOE_LOG, RANKED_LOG] {
        let views = derive_n_times(name, 3);
        assert_eq!(views[0], views[1], "{name} run 1 != run 2");
        assert_eq!(views[1], views[2], "{name} run 2 != run 3");
        assert!(!views[0].opponent_played.is_empty(), "{name} empty played");
    }
}
