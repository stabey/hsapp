use hs_advisor_api::{ActionKind, LegalAction, ServerActionRef};
use hs_planner::{legal_action_to_sim, SimActionMappingError};
use hs_sim::SimAction;
use hs_types::EntityId;

fn legal(
    kind: ActionKind,
    source: Option<i32>,
    target: Option<i32>,
    sub_option: Option<u32>,
    position: Option<u8>,
) -> LegalAction {
    LegalAction {
        server_ref: ServerActionRef {
            options_id: 10,
            option_index: 2,
            sub_option_index: sub_option,
            target: target.map(EntityId),
            position,
        },
        kind,
        source: source.map(EntityId),
    }
}

#[test]
fn every_legal_action_kind_maps_only_its_coordinates() {
    assert_eq!(
        legal_action_to_sim(&legal(
            ActionKind::PlayCard,
            Some(1),
            Some(2),
            Some(3),
            Some(4)
        )),
        Ok(SimAction::PlayCard {
            entity: EntityId(1),
            target: Some(EntityId(2)),
            sub_option: Some(3),
            position: Some(4),
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(ActionKind::Attack, Some(1), Some(2), None, None)),
        Ok(SimAction::Attack {
            attacker: EntityId(1),
            defender: EntityId(2),
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(
            ActionKind::UseHeroPower,
            Some(1),
            Some(2),
            Some(3),
            None
        )),
        Ok(SimAction::UseHeroPower {
            entity: EntityId(1),
            target: Some(EntityId(2)),
            sub_option: Some(3),
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(
            ActionKind::ActivateLocation,
            Some(1),
            Some(2),
            Some(3),
            None
        )),
        Ok(SimAction::ActivateLocation {
            entity: EntityId(1),
            target: Some(EntityId(2)),
            sub_option: Some(3),
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(ActionKind::EndTurn, None, None, None, None)),
        Ok(SimAction::EndTurn)
    );
}

#[test]
fn missing_source_target_and_unknown_are_explicit_mapping_errors() {
    assert_eq!(
        legal_action_to_sim(&legal(ActionKind::PlayCard, None, None, None, None)),
        Err(SimActionMappingError::MissingSource {
            kind: ActionKind::PlayCard
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(ActionKind::Attack, Some(1), None, None, None)),
        Err(SimActionMappingError::MissingTarget {
            kind: ActionKind::Attack
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(ActionKind::Attack, Some(1), Some(2), Some(3), None)),
        Err(SimActionMappingError::UnsupportedSubOption {
            kind: ActionKind::Attack
        })
    );
    assert_eq!(
        legal_action_to_sim(&legal(ActionKind::Unknown, Some(1), Some(2), None, None)),
        Err(SimActionMappingError::UnknownKind)
    );
}
