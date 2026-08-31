//! Shared deterministic search-node primitives.
//!
//! Lethal and Beam searches must derive identity, ordering, and random seeds
//! from the same canonical action path.  This module deliberately contains no
//! simulator, wall-clock, or container-iteration state.

use hs_advisor_api::{ActionKind, ActionOrigin, LegalAction, PlannedAction, ServerActionRef};
use hs_types::EntityId;
use serde::{Deserialize, Serialize};

use crate::legal_actions::ActionTemplate;

/// Version of the platform-independent path-to-seed encoding.
pub const CANONICAL_PATH_SEED_VERSION: u16 = 1;

/// Domain separator for planner path seeds ("hs-plan" plus version 1).
const PATH_SEED_DOMAIN: u64 = 0x6873_2d70_6c61_6e01;

/// One fully ordered action coordinate in a simulated route.
///
/// `origin` is identity rather than authority: only [`LegalAction`] values in
/// the prepared root set may be executed against the live game.  Follow-ups
/// remain simulator-only even though they retain their source Options
/// coordinates for deterministic ordering.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CanonicalActionKey {
    origin: ActionOrigin,
    server_ref: ServerActionRef,
    kind: ActionKind,
    source: Option<EntityId>,
}

impl CanonicalActionKey {
    pub const fn from_server_action(action: LegalAction) -> Self {
        Self {
            origin: ActionOrigin::Server,
            server_ref: action.server_ref,
            kind: action.kind,
            source: action.source,
        }
    }

    pub const fn simulated(
        kind: ActionKind,
        source: Option<EntityId>,
        server_ref: ServerActionRef,
    ) -> Self {
        Self {
            origin: ActionOrigin::Simulated,
            server_ref,
            kind,
            source,
        }
    }

    pub const fn origin(self) -> ActionOrigin {
        self.origin
    }

    pub const fn server_ref(self) -> ServerActionRef {
        self.server_ref
    }

    pub const fn kind(self) -> ActionKind {
        self.kind
    }

    pub const fn source(self) -> Option<EntityId> {
        self.source
    }

    /// Convert a simulator-only coordinate into the public follow-up shape.
    /// A server root is intentionally rejected instead of being relabelled.
    pub fn planned_action(self) -> Option<PlannedAction> {
        (self.origin == ActionOrigin::Simulated).then(|| {
            PlannedAction::simulated(
                self.kind,
                self.source,
                self.server_ref.target,
                self.server_ref.sub_option_index,
                self.server_ref.position,
            )
        })
    }

    pub(crate) fn from_template(template: &ActionTemplate, position: Option<u8>) -> Self {
        Self::simulated(
            template.kind(),
            template.source(),
            template.server_ref(position),
        )
    }
}

/// Canonical full route used for ordering, golden output, and path-local RNG.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalActionPath(Vec<CanonicalActionKey>);

impl CanonicalActionPath {
    pub fn from_root(root: LegalAction) -> Self {
        Self(vec![CanonicalActionKey::from_server_action(root)])
    }

    pub fn push(&mut self, action: CanonicalActionKey) {
        self.0.push(action);
    }

    pub fn as_slice(&self) -> &[CanonicalActionKey] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &CanonicalActionKey> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Derive one seed without consuming shared RNG state.  Sibling traversal
    /// order therefore cannot change another route's random result.
    pub fn seed(&self, root_seed: u64) -> u64 {
        seed_for_path(root_seed, self)
    }
}

/// Platform-independent, versioned path seed derivation.
pub fn seed_for_path(root_seed: u64, path: &CanonicalActionPath) -> u64 {
    let mut hash = root_seed ^ PATH_SEED_DOMAIN;
    hash = mix_seed(hash, u64::from(CANONICAL_PATH_SEED_VERSION));
    for action in path.as_slice() {
        hash = mix_seed(hash, origin_tag(action.origin));
        hash = mix_seed(hash, action.server_ref.options_id);
        hash = mix_seed(hash, u64::from(action.server_ref.option_index));
        hash = mix_seed(
            hash,
            option_u32(action.server_ref.sub_option_index, 0xa5a5_a5a5),
        );
        hash = mix_seed(hash, option_entity(action.source, 0x0101_0101));
        hash = mix_seed(hash, option_entity(action.server_ref.target, 0x0202_0202));
        hash = mix_seed(
            hash,
            action
                .server_ref
                .position
                .map_or(0x0303_0303, |value| u64::from(value) | (1_u64 << 32)),
        );
        hash = mix_seed(hash, action_kind_tag(action.kind));
    }
    avalanche(hash)
}

const fn origin_tag(origin: ActionOrigin) -> u64 {
    match origin {
        ActionOrigin::Server => 1,
        ActionOrigin::Simulated => 2,
    }
}

fn option_u32(value: Option<u32>, none: u64) -> u64 {
    value.map_or(none, |value| u64::from(value) | (1_u64 << 32))
}

fn option_entity(value: Option<EntityId>, none: u64) -> u64 {
    value.map_or(none, |value| (i64::from(value.0) as u64) ^ (1_u64 << 63))
}

const fn action_kind_tag(kind: ActionKind) -> u64 {
    match kind {
        ActionKind::PlayCard => 1,
        ActionKind::Attack => 2,
        ActionKind::UseHeroPower => 3,
        ActionKind::ActivateLocation => 4,
        ActionKind::EndTurn => 5,
        ActionKind::Unknown => 6,
    }
}

fn mix_seed(hash: u64, value: u64) -> u64 {
    avalanche(hash ^ value.wrapping_add(0x9e37_79b9_7f4a_7c15))
}

fn avalanche(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
