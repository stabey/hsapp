#![allow(dead_code)]

use std::sync::Arc;

use hs_effect_ir::EffectProgram;
use hs_log_core::DomainEvent;
use hs_observed_state::{reduce, ObservedGameState, OptionAvailability, OptionSummary};
use hs_planner::PlannerResources;
use hs_pools::PoolSnapshot;
use hs_sim::{CardLookup, PoolResolver, RuleLookup, SimCardDefinition, SimError};
use hs_types::{BuildId, CardId, EntityId, GameId, GamePhase, PlayerId, PoolId};

pub const BUILD: BuildId = BuildId(123_456);
pub const FRIENDLY: PlayerId = PlayerId(1);

pub struct EmptyRules;

impl RuleLookup for EmptyRules {
    fn program_for(&self, _card_id: &CardId) -> Option<&EffectProgram> {
        None
    }
}

pub struct EmptyCards;

impl CardLookup for EmptyCards {
    fn sim_card(&self, _build: BuildId, _card_id: &CardId) -> Option<SimCardDefinition> {
        None
    }
}

pub struct EmptyPools;

impl PoolResolver for EmptyPools {
    fn snapshot(&self, _pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        Err(SimError::NotImplemented("empty planner test pool"))
    }
}

pub fn resources(build_id: BuildId) -> PlannerResources {
    PlannerResources::new(
        build_id,
        Arc::new(EmptyRules),
        Arc::new(EmptyCards),
        Arc::new(EmptyPools),
    )
}

pub fn end_turn_option(index: u32) -> OptionSummary {
    let mut option = OptionSummary::default();
    option.index = index;
    option.option_type = "END_TURN".into();
    option.availability = OptionAvailability::Allowed;
    option
}

pub fn valid_state() -> ObservedGameState {
    let mut state = ObservedGameState::default();
    state.build_id = Some(BUILD);
    state.game_id = Some(GameId(77));
    state.phase = GamePhase::Playing;
    state.friendly_player = Some(FRIENDLY);
    state.current_player = Some(FRIENDLY);
    state.revision = 40;
    state = reduce(
        &state,
        &DomainEvent::OptionsUpdated {
            id: 91,
            options: vec![end_turn_option(3)],
        },
    )
    .expect("valid options update");
    state
}

pub fn with_open_block(state: &ObservedGameState) -> ObservedGameState {
    reduce(
        state,
        &DomainEvent::BlockOpened {
            block_type: "POWER".into(),
            source: Some(EntityId(1)),
            target: None,
            effect_card_id: None,
        },
    )
    .expect("valid block")
}

pub fn with_friendly_choice(state: &ObservedGameState) -> ObservedGameState {
    reduce(
        state,
        &DomainEvent::ChoicesUpdated {
            id: 55,
            task_list: Some(8),
            player: Some(FRIENDLY),
            player_entity: None,
            player_raw: String::new(),
            choice_type: "DISCOVER".into(),
            min: 1,
            max: 1,
            source: None,
            source_raw: String::new(),
            entities: vec![EntityId(300)],
        },
    )
    .expect("valid choice")
}
