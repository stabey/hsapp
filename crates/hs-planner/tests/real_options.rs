//! Real-log acceptance for the server Options adapter.

use hs_advisor_api::{ActionKind, LegalAction, ServerActionRef};
use hs_log_core::{
    EventNormalizer, OptionAvailability, OptionEntry, Packet, PowerEventNormalizer, PowerLogParser,
};
use hs_log_record::{ParseStatus, RawLogRecord};
use hs_observed_state::{reduce, ObservedGameState, OptionSummary, OptionsState};
use hs_planner::{is_current_action, legal_actions};
use hs_types::{CardType, EntityId, LogFileId, LogSessionId, Zone};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Default, Eq, PartialEq)]
struct ReplayStats {
    options: usize,
    sends: usize,
    selected_none: usize,
    selected_end_turn_invalid: usize,
    selected_legacy: usize,
    selected_sub_option: usize,
    nonzero_position: usize,
    nonzero_target: usize,
    allowed_targets: usize,
    blocked_targets: usize,
    legacy_targets: usize,
}

fn real_log_records(name: &str) -> Vec<RawLogRecord> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../tests/logs/real/{name}"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(index, line)| RawLogRecord {
            session_id: LogSessionId(1),
            file_id: LogFileId(1),
            byte_offset: index as u64,
            timestamp: None,
            raw_line: Arc::from(line),
            parse_status: ParseStatus::Pending,
        })
        .collect()
}

fn count_targets(entry: &OptionEntry, stats: &mut ReplayStats) {
    for target in &entry.target_entries {
        match target.availability {
            OptionAvailability::Allowed => stats.allowed_targets += 1,
            OptionAvailability::Blocked => stats.blocked_targets += 1,
            OptionAvailability::LegacyUnspecified => stats.legacy_targets += 1,
        }
    }
    for sub_option in &entry.sub_options {
        count_targets(sub_option, stats);
    }
}

fn selected_node(option: &OptionSummary, sub_option: i32) -> &OptionSummary {
    if sub_option < 0 {
        return option;
    }
    option
        .sub_options
        .iter()
        .find(|candidate| candidate.index == sub_option as u32)
        .unwrap_or_else(|| {
            panic!(
                "missing selected subOption {} under option {}",
                sub_option, option.index
            )
        })
}

fn assert_blocked_nodes_absent(options: &OptionsState, actions: &[LegalAction]) {
    for option in &options.options {
        if !option.availability.is_selectable() {
            assert!(
                actions
                    .iter()
                    .all(|action| action.server_ref.option_index != option.index),
                "blocked top-level option entered actions: {option:?}"
            );
            continue;
        }

        if option.sub_options.is_empty() {
            assert_blocked_targets_absent(option.index, None, option, actions);
            continue;
        }

        for sub in &option.sub_options {
            if !sub.availability.is_selectable() || !sub.sub_options.is_empty() {
                assert!(
                    actions.iter().all(|action| {
                        action.server_ref.option_index != option.index
                            || action.server_ref.sub_option_index != Some(sub.index)
                    }),
                    "blocked or recursively incomplete sub-option entered actions: {sub:?}"
                );
                continue;
            }
            assert_blocked_targets_absent(option.index, Some(sub.index), sub, actions);
        }
    }
}

fn assert_blocked_targets_absent(
    option_index: u32,
    sub_option_index: Option<u32>,
    node: &OptionSummary,
    actions: &[LegalAction],
) {
    let branch_actions = || {
        actions.iter().filter(|action| {
            action.server_ref.option_index == option_index
                && action.server_ref.sub_option_index == sub_option_index
        })
    };
    if !node.target_entries.is_empty()
        && node
            .target_entries
            .iter()
            .all(|target| !target.availability.is_selectable())
    {
        assert_eq!(branch_actions().count(), 0, "all targets are blocked");
    }
    for blocked in node
        .target_entries
        .iter()
        .filter(|target| !target.availability.is_selectable())
    {
        let Some(entity) = blocked.entity else {
            continue;
        };
        let has_selectable_duplicate = node
            .target_entries
            .iter()
            .any(|target| target.entity == Some(entity) && target.availability.is_selectable());
        if !has_selectable_duplicate {
            assert!(
                branch_actions().all(|action| action.server_ref.target != Some(entity)),
                "blocked target entered actions: {blocked:?}"
            );
        }
    }
}

fn expected_selected_kind(
    state: &ObservedGameState,
    option: &OptionSummary,
    selected_ref: &ServerActionRef,
) -> ActionKind {
    if option.option_type.eq_ignore_ascii_case("END_TURN") {
        return ActionKind::EndTurn;
    }
    if option.option_type.eq_ignore_ascii_case("ATTACK") {
        return ActionKind::Attack;
    }

    let source = option
        .entity
        .expect("selected non-end-turn option has source");
    let source = state.entity(source).expect("selected source is observed");
    match (source.zone, source.card_type()) {
        (
            Zone::Hand,
            Some(
                CardType::Minion
                | CardType::Spell
                | CardType::Weapon
                | CardType::Location
                | CardType::Hero,
            ),
        ) => ActionKind::PlayCard,
        (Zone::Play, Some(CardType::HeroPower)) => ActionKind::UseHeroPower,
        (Zone::Play, Some(CardType::Location)) => ActionKind::ActivateLocation,
        (Zone::Play, Some(CardType::Hero | CardType::Minion))
            if selected_ref.target.is_some() && selected_ref.sub_option_index.is_none() =>
        {
            ActionKind::Attack
        }
        other => panic!(
            "selected option has no expected action kind: type={} source={other:?}",
            option.option_type
        ),
    }
}

fn replay(name: &str) -> ReplayStats {
    let packets = PowerLogParser::parse_records(real_log_records(name));
    let mut normalizer = PowerEventNormalizer::new();
    let mut state = ObservedGameState::default();
    let mut stats = ReplayStats::default();

    for packet in packets {
        let publishes_options = matches!(&packet, Packet::Options(_));
        if let Packet::Options(options) = &packet {
            stats.options += 1;
            for option in &options.options {
                count_targets(option, &mut stats);
            }
        }

        if let Packet::SendOption(send) = &packet {
            stats.sends += 1;
            stats.selected_sub_option += usize::from(send.sub_option >= 0);
            stats.nonzero_position += usize::from(send.position > 0);
            stats.nonzero_target += usize::from(send.target > 0);

            let options = state
                .options
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: SendOption without current Options: {send:?}"));
            let option_index = u32::try_from(send.option)
                .unwrap_or_else(|_| panic!("{name}: negative selected option: {send:?}"));
            let option = options
                .options
                .iter()
                .find(|candidate| candidate.index == option_index)
                .unwrap_or_else(|| panic!("{name}: selected option missing: {send:?}"));
            assert!(
                option.availability.is_selectable(),
                "{name}: selected blocked option: {send:?} {option:?}"
            );
            match option.raw_error.as_deref() {
                Some(error) if error.eq_ignore_ascii_case("NONE") => stats.selected_none += 1,
                Some(error)
                    if option.option_type.eq_ignore_ascii_case("END_TURN")
                        && error.eq_ignore_ascii_case("INVALID") =>
                {
                    stats.selected_end_turn_invalid += 1;
                }
                None | Some("") => stats.selected_legacy += 1,
                Some(error) => panic!("{name}: selected unexpected error {error}: {send:?}"),
            }

            let selected = selected_node(option, send.sub_option);
            assert!(
                selected.availability.is_selectable(),
                "{name}: selected blocked subOption: {send:?} {selected:?}"
            );
            if send.target > 0 {
                let target = selected
                    .target_entries
                    .iter()
                    .find(|target| target.entity == Some(EntityId(send.target)))
                    .unwrap_or_else(|| panic!("{name}: selected target missing: {send:?}"));
                assert!(
                    target.availability.is_selectable(),
                    "{name}: selected blocked target: {send:?} {target:?}"
                );
            }

            let expected_ref = ServerActionRef {
                options_id: options.id,
                option_index,
                sub_option_index: (send.sub_option >= 0).then_some(send.sub_option as u32),
                target: (send.target > 0).then_some(EntityId(send.target)),
                position: (send.position > 0).then_some(send.position as u8),
            };
            let actions = legal_actions(&state).unwrap_or_else(|error| {
                panic!(
                    "{name}: legal actions unavailable: {error:?}; send={send:?}; choices={:?}",
                    state.choices
                )
            });
            assert_eq!(
                actions,
                legal_actions(&state).expect("same state must remain available"),
                "{name}: same input changed legal action IDs or order"
            );
            assert_blocked_nodes_absent(options, &actions.actions);
            let selected_action = actions
                .actions
                .iter()
                .find(|action| action.server_ref == expected_ref)
                .unwrap_or_else(|| {
                    panic!(
                        "{name}: sent action not found: {send:?}; expected={expected_ref:?}; diagnostics={:?}",
                        actions.diagnostics
                    )
                });
            assert_eq!(
                selected_action.kind,
                expected_selected_kind(&state, option, &expected_ref),
                "{name}: selected action was misclassified: {send:?}"
            );
            let game_id = state.game_id.expect("real Options must belong to a game");
            assert!(is_current_action(
                &state,
                game_id,
                options.issued_revision,
                &expected_ref
            ));
            assert!(!is_current_action(
                &state,
                game_id,
                options.issued_revision.saturating_add(1),
                &expected_ref
            ));
        }

        for event in normalizer.normalize(&packet).expect("normalize packet") {
            state = reduce(&state, &event).expect("reduce event");
        }
        if publishes_options {
            let current = state
                .options
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: Options packet did not enter observed state"));
            let projected = legal_actions(&state).unwrap_or_else(|error| {
                panic!("{name}: published Options cannot be projected: {error:?}")
            });
            assert_blocked_nodes_absent(current, &projected.actions);
        }
    }

    stats
}

#[test]
fn all_real_send_options_map_to_selectable_server_actions() {
    let access = replay("186852_hearthstone_access_eoe.power.log");
    assert_eq!(
        (
            access.options,
            access.sends,
            access.selected_none,
            access.selected_end_turn_invalid,
            access.selected_legacy,
            access.selected_sub_option,
            access.nonzero_position,
            access.nonzero_target,
        ),
        (16, 15, 11, 4, 0, 4, 2, 0)
    );

    let friendly = replay("friendly_player_id_is_1.power.log");
    assert_eq!(
        (
            friendly.options,
            friendly.sends,
            friendly.selected_none,
            friendly.selected_end_turn_invalid,
            friendly.selected_legacy,
            friendly.selected_sub_option,
            friendly.nonzero_position,
            friendly.nonzero_target,
        ),
        (23, 23, 0, 0, 23, 0, 6, 6)
    );

    let ranked = replay("2026-08-21-ranked-standard-loss.power.log");
    assert_eq!(
        (
            ranked.options,
            ranked.sends,
            ranked.selected_none,
            ranked.selected_end_turn_invalid,
            ranked.selected_legacy,
            ranked.selected_sub_option,
            ranked.nonzero_position,
            ranked.nonzero_target,
        ),
        (81, 78, 69, 9, 0, 0, 10, 17)
    );

    assert_eq!(
        access.allowed_targets + friendly.allowed_targets + ranked.allowed_targets,
        545
    );
    assert_eq!(
        access.blocked_targets + friendly.blocked_targets + ranked.blocked_targets,
        452
    );
    assert_eq!(
        access.legacy_targets + friendly.legacy_targets + ranked.legacy_targets,
        90
    );
}
