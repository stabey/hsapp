use hs_advisor_api::{ActionKind, ActionOrigin, LegalAction, ServerActionRef};
use hs_planner::{
    seed_for_path, CanonicalActionKey, CanonicalActionPath, CANONICAL_PATH_SEED_VERSION,
};
use hs_types::EntityId;

fn root() -> LegalAction {
    LegalAction {
        server_ref: ServerActionRef {
            options_id: 17,
            option_index: 3,
            sub_option_index: Some(1),
            target: Some(EntityId(99)),
            position: Some(2),
        },
        kind: ActionKind::PlayCard,
        source: Some(EntityId(42)),
    }
}

#[test]
fn path_seed_encoding_is_versioned_and_golden() {
    assert_eq!(CANONICAL_PATH_SEED_VERSION, 1);
    let path = CanonicalActionPath::from_root(root());
    assert_eq!(seed_for_path(123, &path), 14_897_648_478_825_944_010);
    assert_eq!(path.seed(123), seed_for_path(123, &path));
}

#[test]
fn every_coordinate_and_origin_changes_the_seed() {
    let root = root();
    let baseline = CanonicalActionPath::from_root(root).seed(7);
    let variants = [
        CanonicalActionKey::simulated(root.kind, root.source, root.server_ref),
        CanonicalActionKey::simulated(
            root.kind,
            root.source,
            ServerActionRef {
                target: None,
                ..root.server_ref
            },
        ),
        CanonicalActionKey::simulated(ActionKind::Attack, root.source, root.server_ref),
        CanonicalActionKey::simulated(root.kind, None, root.server_ref),
    ];

    for variant in variants {
        let mut path = CanonicalActionPath::from_root(root);
        path.push(variant);
        assert_ne!(path.seed(7), baseline);
    }
}

#[test]
fn only_simulated_keys_project_to_follow_up_actions() {
    let server = CanonicalActionKey::from_server_action(root());
    assert_eq!(server.origin(), ActionOrigin::Server);
    assert!(server.planned_action().is_none());

    let simulated = CanonicalActionKey::simulated(
        ActionKind::Attack,
        Some(EntityId(7)),
        ServerActionRef {
            options_id: 17,
            option_index: 4,
            sub_option_index: None,
            target: Some(EntityId(8)),
            position: None,
        },
    );
    let planned = simulated.planned_action().expect("simulated follow-up");
    assert_eq!(planned.origin(), ActionOrigin::Simulated);
    assert_eq!(planned.kind, ActionKind::Attack);
    assert_eq!(planned.source, Some(EntityId(7)));
    assert_eq!(planned.target, Some(EntityId(8)));
}

#[test]
fn canonical_path_order_uses_origin_and_full_server_coordinates() {
    let root = root();
    let server = CanonicalActionKey::from_server_action(root);
    let simulated = CanonicalActionKey::simulated(root.kind, root.source, root.server_ref);
    assert!(server < simulated);

    let mut left = CanonicalActionPath::from_root(root);
    left.push(CanonicalActionKey::simulated(
        ActionKind::Attack,
        Some(EntityId(4)),
        ServerActionRef {
            option_index: 10,
            ..root.server_ref
        },
    ));
    let mut right = CanonicalActionPath::from_root(root);
    right.push(CanonicalActionKey::simulated(
        ActionKind::Attack,
        Some(EntityId(4)),
        ServerActionRef {
            option_index: 11,
            ..root.server_ref
        },
    ));
    assert!(left < right);
}
