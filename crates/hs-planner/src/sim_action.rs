//! Lossless structural mapping from server-backed actions into `hs-sim`.

use hs_advisor_api::{ActionKind, LegalAction};
use hs_sim::SimAction;
use thiserror::Error;

use crate::legal_actions::ActionTemplate;

/// A legal action lacked a field required by the corresponding simulator
/// variant, or carried the deliberately unclassified `Unknown` kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum SimActionMappingError {
    #[error("{kind:?} action is missing its source entity")]
    MissingSource { kind: ActionKind },
    #[error("{kind:?} action is missing its target entity")]
    MissingTarget { kind: ActionKind },
    #[error("{kind:?} action carries a sub-option that has no simulator mapping")]
    UnsupportedSubOption { kind: ActionKind },
    #[error("unknown legal action kind cannot be simulated")]
    UnknownKind,
}

/// Map only action coordinates. Controller, zone, Taunt, availability, and all
/// other legality remain owned by `legal_actions` and `hs-sim`.
pub fn legal_action_to_sim(action: &LegalAction) -> Result<SimAction, SimActionMappingError> {
    coordinates_to_sim(
        action.kind,
        action.source,
        action.server_ref.target,
        action.server_ref.sub_option_index,
        action.server_ref.position,
    )
}

/// Map a conservative continuation through the exact same coordinate owner as
/// a current server action. Template membership does not grant legality;
/// `apply_action` validates the derived state afterward.
pub(crate) fn template_action_to_sim(
    template: &ActionTemplate,
    position: Option<u8>,
) -> Result<SimAction, SimActionMappingError> {
    coordinates_to_sim(
        template.kind(),
        template.source(),
        template.target(),
        template.sub_option_index(),
        position,
    )
}

fn coordinates_to_sim(
    kind: ActionKind,
    source: Option<hs_types::EntityId>,
    target: Option<hs_types::EntityId>,
    sub_option: Option<u32>,
    position: Option<u8>,
) -> Result<SimAction, SimActionMappingError> {
    let required_source = || source.ok_or(SimActionMappingError::MissingSource { kind });
    let required_target = || target.ok_or(SimActionMappingError::MissingTarget { kind });

    match kind {
        ActionKind::PlayCard => Ok(SimAction::PlayCard {
            entity: required_source()?,
            target,
            sub_option,
            position,
        }),
        ActionKind::Attack => {
            if sub_option.is_some() {
                return Err(SimActionMappingError::UnsupportedSubOption { kind });
            }
            Ok(SimAction::Attack {
                attacker: required_source()?,
                defender: required_target()?,
            })
        }
        ActionKind::UseHeroPower => Ok(SimAction::UseHeroPower {
            entity: required_source()?,
            target,
            sub_option,
        }),
        ActionKind::ActivateLocation => Ok(SimAction::ActivateLocation {
            entity: required_source()?,
            target,
            sub_option,
        }),
        ActionKind::EndTurn => Ok(SimAction::EndTurn),
        ActionKind::Unknown => Err(SimActionMappingError::UnknownKind),
    }
}
