//! The single adapter from observed server Options to stable legal actions.

use hs_advisor_api::{ActionKind, LegalAction, ServerActionRef};
use hs_observed_state::{
    EntityState, ObservedGameState, OptionAvailability, OptionSummary, OptionTargetSummary,
};
use hs_types::{CardType, EntityId, GameId, PlayerId, Zone};
use serde::{Deserialize, Serialize};

const MAX_BOARD_SLOTS: usize = 7;

#[derive(Clone, Copy)]
struct ClassificationInput<'a> {
    option_index: u32,
    option_type: &'a str,
    source: Option<EntityId>,
    has_targets: bool,
    has_sub_options: bool,
}

#[derive(Clone, Copy)]
struct ClassifiedSource<'a> {
    kind: ActionKind,
    source: Option<EntityId>,
    source_state: Option<&'a EntityState>,
}

struct ExpansionInput<'a> {
    options_id: u64,
    option_index: u32,
    sub_option_index: Option<u32>,
    classified: ClassifiedSource<'a>,
    target_entries: &'a [OptionTargetSummary],
}

/// Why a server option could not be projected into a selectable action.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalActionDiagnosticReason {
    MissingSource,
    SourceNotObserved,
    SourceNotFriendly,
    SourceTypeUnknown,
    ActionKindUnknown,
    TargetNotObserved,
    MissingTargets,
    NestedSubOptionsUnsupported,
    AttackSubOptionUnsupported,
    FutureActionKindAmbiguous,
    BoardFull,
    BoardPositionsIncomplete,
}

/// Stable per-option diagnostic. Blocked server nodes are not diagnostics:
/// their original error remains available in the observed Options tree.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct LegalActionDiagnostic {
    pub option_index: u32,
    pub sub_option_index: Option<u32>,
    pub target_index: Option<u32>,
    pub reason: LegalActionDiagnosticReason,
}

/// Current legal actions together with the identity of the Options snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LegalActionSet {
    pub game_id: GameId,
    pub options_id: u64,
    pub issued_revision: u64,
    pub actions: Vec<LegalAction>,
    pub diagnostics: Vec<LegalActionDiagnostic>,
}

/// Same-Options action coordinates retained for conservative depth 1+
/// simulation. This type is crate-private so a template can never be mistaken
/// for a currently server-backed [`LegalAction`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionTemplateSet {
    game_id: GameId,
    options_id: u64,
    issued_revision: u64,
    templates: Vec<ActionTemplate>,
    /// Coordinates from this Options packet that could not be preserved as a
    /// conservative continuation. Search must therefore never call an empty
    /// result exhaustive merely because those coordinates are absent.
    projection_gaps: Vec<LegalActionDiagnostic>,
}

impl ActionTemplateSet {
    pub(crate) fn templates(&self) -> &[ActionTemplate] {
        &self.templates
    }

    pub(crate) fn game_id(&self) -> GameId {
        self.game_id
    }

    pub(crate) fn options_id(&self) -> u64 {
        self.options_id
    }

    pub(crate) fn issued_revision(&self) -> u64 {
        self.issued_revision
    }

    pub(crate) fn has_projection_gaps(&self) -> bool {
        !self.projection_gaps.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum TemplatePosition {
    None,
    DynamicBoardSlot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoardPositionProjection {
    Available { occupied: usize },
    Full,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum TemplateGateLevel {
    Top,
    Sub(u32),
    Target(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TemplateGate {
    pub(crate) level: TemplateGateLevel,
    pub(crate) availability: OptionAvailability,
    pub(crate) requirement: Option<String>,
    pub(crate) error_param: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ActionTemplate {
    options_id: u64,
    option_index: u32,
    sub_option_index: Option<u32>,
    target_index: Option<u32>,
    kind: ActionKind,
    source: Option<EntityId>,
    target: Option<EntityId>,
    position: TemplatePosition,
    gates: Vec<TemplateGate>,
}

impl ActionTemplate {
    pub(crate) fn kind(&self) -> ActionKind {
        self.kind
    }

    pub(crate) fn source(&self) -> Option<EntityId> {
        self.source
    }

    pub(crate) fn target(&self) -> Option<EntityId> {
        self.target
    }

    pub(crate) fn sub_option_index(&self) -> Option<u32> {
        self.sub_option_index
    }

    /// A blocked server requirement may be retried only when the shared
    /// simulator validates the same fact. Card-specific target requirements
    /// remain templates for diagnostics but never become executable guesses.
    pub(crate) fn has_only_recheckable_gates(&self) -> bool {
        // The simulator completely revalidates ordinary combat. For cards,
        // Hero Powers, and Locations it currently lacks complete card-specific
        // play/target requirement metadata, so even an Allowed/NONE server
        // gate is only proof for the root snapshot, not a derived state.
        if !matches!(self.kind, ActionKind::Attack | ActionKind::EndTurn) {
            return false;
        }
        self.gates.iter().all(|gate| {
            if gate.availability.is_selectable() {
                return true;
            }
            if gate
                .error_param
                .as_deref()
                .is_some_and(|parameter| !parameter.is_empty())
            {
                // The shared simulator has no generic interpretation for a
                // parameterized server requirement, so its meaning cannot be
                // reconstructed from the requirement name alone.
                return false;
            }
            let requirement = gate.requirement.as_deref().unwrap_or_default();
            matches!(
                (self.kind, gate.level, requirement),
                (
                    ActionKind::Attack,
                    TemplateGateLevel::Top,
                    "REQ_NOT_MINION_JUST_PLAYED" | "REQ_ATTACK_GREATER_THAN_0",
                ) | (
                    ActionKind::Attack,
                    TemplateGateLevel::Target(_),
                    "REQ_ENEMY_TARGET" | "REQ_TARGET_TAUNTER",
                )
            )
        })
    }

    /// Expand only the mechanical board-position coordinate. No new source,
    /// target, or server choice is synthesized.
    pub(crate) fn positions_for(&self, state: &hs_sim::SimGameState) -> Vec<Option<u8>> {
        if self.position == TemplatePosition::None {
            return vec![None];
        }
        let Some(controller) = self
            .source
            .and_then(|source| state.entity(source))
            .and_then(|entity| entity.controller)
        else {
            return Vec::new();
        };
        let mut observed_positions = Vec::new();
        for entity in state
            .entities
            .values()
            .filter(|entity| entity.controller == Some(controller) && entity.zone == Zone::Play)
        {
            match entity.card_type {
                Some(CardType::Minion | CardType::Location) => {
                    let Some(position) =
                        entity.action.zone_position.filter(|position| *position > 0)
                    else {
                        return Vec::new();
                    };
                    observed_positions.push(position);
                }
                None | Some(CardType::Unknown(_))
                    if entity.card_id.is_none()
                        && entity
                            .action
                            .zone_position
                            .is_none_or(|position| position == 0) => {}
                None | Some(CardType::Unknown(_)) => return Vec::new(),
                Some(_) => {}
            }
        }
        observed_positions.sort_unstable();
        if observed_positions
            .iter()
            .enumerate()
            .any(|(index, position)| usize::from(*position) != index + 1)
        {
            return Vec::new();
        }
        let occupied = observed_positions.len();
        if occupied >= MAX_BOARD_SLOTS {
            return Vec::new();
        }
        (1..=occupied + 1)
            .map(|position| Some(position as u8))
            .collect()
    }

    pub(crate) fn server_ref(&self, position: Option<u8>) -> ServerActionRef {
        ServerActionRef {
            options_id: self.options_id,
            option_index: self.option_index,
            sub_option_index: self.sub_option_index,
            target: self.target,
            position,
        }
    }
}

/// Conservative reasons for not exposing any current server action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalActionsUnavailable {
    MissingGame,
    MissingOptions,
    MissingFriendlyPlayer,
    PendingChoices,
}

/// Convert the current authoritative Options snapshot into stable actions.
///
/// This is the only owner of availability filtering, action classification,
/// target/sub-option expansion, board positions, and action ordering.
pub fn legal_actions(state: &ObservedGameState) -> Result<LegalActionSet, LegalActionsUnavailable> {
    legal_actions_with_templates(state).map(|(legal, _)| legal)
}

/// Project both currently selectable roots and the conservative action
/// templates carried by the same complete Options packet.
///
/// Templates deliberately retain blocked option/sub-option/target coordinates
/// for depth 1+ simulation. They are never exposed as current LegalActions;
/// `hs-sim::apply_action` must prove them available again in each derived state.
pub(crate) fn legal_actions_with_templates(
    state: &ObservedGameState,
) -> Result<(LegalActionSet, ActionTemplateSet), LegalActionsUnavailable> {
    let game_id = state.game_id.ok_or(LegalActionsUnavailable::MissingGame)?;
    let options = state
        .options
        .as_ref()
        .ok_or(LegalActionsUnavailable::MissingOptions)?;
    let friendly = state
        .friendly_player
        .ok_or(LegalActionsUnavailable::MissingFriendlyPlayer)?;
    if state
        .choices
        .iter()
        .any(|choice| choice.player.is_none() || choice.player == Some(friendly))
    {
        return Err(LegalActionsUnavailable::PendingChoices);
    }

    let mut actions = Vec::new();
    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut template_diagnostics = Vec::new();
    let mut entries = options.options.iter().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.index);

    for entry in entries {
        let entry_selectable = entry.availability.is_selectable();
        let classification = ClassificationInput {
            option_index: entry.index,
            option_type: &entry.option_type,
            source: entry.entity,
            has_targets: !entry.target_entries.is_empty(),
            has_sub_options: !entry.sub_options.is_empty(),
        };
        let mut classification_diagnostics = Vec::new();
        let classified = classify_action(
            state,
            friendly,
            classification,
            &mut classification_diagnostics,
        );
        if entry_selectable {
            diagnostics.extend(classification_diagnostics.iter().cloned());
        }
        // A classification failure also means the same-packet continuation
        // projection has a hole, including for currently selectable nodes.
        // A source explicitly controlled by the opponent is the exception:
        // real Options packets retain the opponent's REQ_YOUR_TURN nodes,
        // which can never be our current-turn continuation.
        let definitely_opponent = entry
            .entity
            .and_then(|source| state.entity(source))
            .and_then(|entity| entity.controller)
            .is_some_and(|controller| controller != friendly);
        template_diagnostics.extend(classification_diagnostics.into_iter().filter(|diagnostic| {
            !definitely_opponent
                || diagnostic.reason != LegalActionDiagnosticReason::SourceNotFriendly
        }));
        let Some(classified) = classified else {
            continue;
        };
        record_requirement_kind_gaps(entry, classified.kind, &mut template_diagnostics);

        if entry.sub_options.is_empty() {
            expand_template_node(
                state,
                options.id,
                entry,
                None,
                classified,
                &mut templates,
                &mut template_diagnostics,
            );
            if !entry_selectable {
                continue;
            }
            expand_node(
                state,
                friendly,
                ExpansionInput {
                    options_id: options.id,
                    option_index: entry.index,
                    sub_option_index: None,
                    classified,
                    target_entries: &entry.target_entries,
                },
                &mut actions,
                &mut diagnostics,
            );
            continue;
        }

        let mut sub_options = entry.sub_options.iter().collect::<Vec<_>>();
        sub_options.sort_by_key(|sub| sub.index);
        for sub in sub_options {
            if !sub.sub_options.is_empty() {
                if entry_selectable && sub.availability.is_selectable() {
                    push_diagnostic(
                        &mut diagnostics,
                        entry.index,
                        Some(sub.index),
                        None,
                        LegalActionDiagnosticReason::NestedSubOptionsUnsupported,
                    );
                }
                push_diagnostic(
                    &mut template_diagnostics,
                    entry.index,
                    Some(sub.index),
                    None,
                    LegalActionDiagnosticReason::NestedSubOptionsUnsupported,
                );
                continue;
            }
            expand_template_node(
                state,
                options.id,
                entry,
                Some(sub),
                classified,
                &mut templates,
                &mut template_diagnostics,
            );
            if !entry_selectable || !sub.availability.is_selectable() {
                continue;
            }
            expand_node(
                state,
                friendly,
                ExpansionInput {
                    options_id: options.id,
                    option_index: entry.index,
                    sub_option_index: Some(sub.index),
                    classified,
                    target_entries: &sub.target_entries,
                },
                &mut actions,
                &mut diagnostics,
            );
        }
    }

    actions.sort_by_key(|action| action.server_ref);
    actions.dedup_by_key(|action| action.server_ref);
    templates.sort_by_key(|template| {
        (
            template.option_index,
            template.sub_option_index,
            template.target,
            template.position,
            template.target_index,
        )
    });
    let mut merged_templates = Vec::<ActionTemplate>::with_capacity(templates.len());
    for mut template in templates {
        if let Some(previous) = merged_templates
            .last_mut()
            .filter(|previous| same_template_identity(previous, &template))
        {
            // Target indexes are parser coordinates, not ServerActionRef
            // identity. If malformed/legacy input repeats one entity under
            // several indexes, retain the union of gates so the merge can
            // only become less executable, never more permissive.
            for gate in template.gates.drain(..) {
                if !previous.gates.contains(&gate) {
                    previous.gates.push(gate);
                }
            }
        } else {
            merged_templates.push(template);
        }
    }
    templates = merged_templates;
    diagnostics.sort();
    diagnostics.dedup();
    template_diagnostics.sort();
    template_diagnostics.dedup();

    Ok((
        LegalActionSet {
            game_id,
            options_id: options.id,
            issued_revision: options.issued_revision,
            actions,
            diagnostics,
        },
        ActionTemplateSet {
            game_id,
            options_id: options.id,
            issued_revision: options.issued_revision,
            templates,
            projection_gaps: template_diagnostics,
        },
    ))
}

/// Check that an action still belongs to the current game and Options issue.
pub fn is_current_action(
    state: &ObservedGameState,
    game_id: GameId,
    issued_revision: u64,
    action_ref: &ServerActionRef,
) -> bool {
    if state.game_id != Some(game_id) {
        return false;
    }
    let Some(options) = state.options.as_ref() else {
        return false;
    };
    if options.id != action_ref.options_id || options.issued_revision != issued_revision {
        return false;
    }
    legal_actions(state).is_ok_and(|set| {
        set.actions
            .iter()
            .any(|action| action.server_ref == *action_ref)
    })
}

fn expand_node(
    state: &ObservedGameState,
    friendly: PlayerId,
    input: ExpansionInput<'_>,
    actions: &mut Vec<LegalAction>,
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) {
    if input.classified.kind == ActionKind::Attack && input.sub_option_index.is_some() {
        push_diagnostic(
            diagnostics,
            input.option_index,
            input.sub_option_index,
            None,
            LegalActionDiagnosticReason::AttackSubOptionUnsupported,
        );
        return;
    }
    if input.classified.kind == ActionKind::Attack && input.target_entries.is_empty() {
        push_diagnostic(
            diagnostics,
            input.option_index,
            input.sub_option_index,
            None,
            LegalActionDiagnosticReason::MissingTargets,
        );
        return;
    }

    let targets = selectable_targets(
        state,
        input.option_index,
        input.sub_option_index,
        input.target_entries,
        diagnostics,
    );
    let positions = legal_positions(
        state,
        friendly,
        input.option_index,
        input.sub_option_index,
        input.classified.kind,
        input.classified.source_state,
        diagnostics,
    );

    for target in targets {
        for &position in &positions {
            actions.push(LegalAction {
                server_ref: ServerActionRef {
                    options_id: input.options_id,
                    option_index: input.option_index,
                    sub_option_index: input.sub_option_index,
                    target,
                    position,
                },
                kind: input.classified.kind,
                source: input.classified.source,
            });
        }
    }
}

fn expand_template_node(
    state: &ObservedGameState,
    options_id: u64,
    entry: &OptionSummary,
    sub: Option<&OptionSummary>,
    classified: ClassifiedSource<'_>,
    templates: &mut Vec<ActionTemplate>,
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) {
    let target_entries = sub.map_or(entry.target_entries.as_slice(), |sub| {
        sub.target_entries.as_slice()
    });
    let sub_option_index = sub.map(|sub| sub.index);
    if classified.kind == ActionKind::Attack && sub_option_index.is_some() {
        push_diagnostic(
            diagnostics,
            entry.index,
            sub_option_index,
            None,
            LegalActionDiagnosticReason::AttackSubOptionUnsupported,
        );
        return;
    }
    if classified.kind == ActionKind::Attack && target_entries.is_empty() {
        push_diagnostic(
            diagnostics,
            entry.index,
            sub_option_index,
            None,
            LegalActionDiagnosticReason::MissingTargets,
        );
        return;
    }

    let mut base_gates = vec![option_gate(TemplateGateLevel::Top, entry)];
    if let Some(sub) = sub {
        base_gates.push(option_gate(TemplateGateLevel::Sub(sub.index), sub));
    }
    let position = if classified.kind == ActionKind::PlayCard
        && classified.source_state.is_some_and(|entity| {
            matches!(
                entity.card_type(),
                Some(CardType::Minion | CardType::Location)
            )
        }) {
        TemplatePosition::DynamicBoardSlot
    } else {
        TemplatePosition::None
    };
    if position == TemplatePosition::DynamicBoardSlot
        && classified
            .source_state
            .and_then(|source| source.controller)
            .is_none_or(|controller| {
                board_position_projection(state, controller) == BoardPositionProjection::Incomplete
            })
    {
        push_diagnostic(
            diagnostics,
            entry.index,
            sub_option_index,
            None,
            LegalActionDiagnosticReason::BoardPositionsIncomplete,
        );
    }

    if target_entries.is_empty() {
        templates.push(ActionTemplate {
            options_id,
            option_index: entry.index,
            sub_option_index,
            target_index: None,
            kind: classified.kind,
            source: classified.source,
            target: None,
            position,
            gates: base_gates,
        });
        return;
    }

    let mut targets = target_entries.iter().collect::<Vec<_>>();
    targets.sort_by_key(|target| target.index);
    for target in targets {
        let Some(entity) = target.entity else {
            push_diagnostic(
                diagnostics,
                entry.index,
                sub_option_index,
                Some(target.index),
                LegalActionDiagnosticReason::TargetNotObserved,
            );
            continue;
        };
        if state.entity(entity).is_none() {
            push_diagnostic(
                diagnostics,
                entry.index,
                sub_option_index,
                Some(target.index),
                LegalActionDiagnosticReason::TargetNotObserved,
            );
            continue;
        }
        let mut gates = base_gates.clone();
        gates.push(target_gate(target));
        templates.push(ActionTemplate {
            options_id,
            option_index: entry.index,
            sub_option_index,
            target_index: Some(target.index),
            kind: classified.kind,
            source: classified.source,
            target: Some(entity),
            position,
            gates,
        });
    }
}

fn option_gate(level: TemplateGateLevel, option: &OptionSummary) -> TemplateGate {
    TemplateGate {
        level,
        availability: option.availability,
        requirement: option.raw_error.clone().or_else(|| option.error.clone()),
        error_param: option.error_param.clone(),
    }
}

fn target_gate(target: &OptionTargetSummary) -> TemplateGate {
    TemplateGate {
        level: TemplateGateLevel::Target(target.index),
        availability: target.availability,
        requirement: target.raw_error.clone().or_else(|| target.error.clone()),
        error_param: target.error_param.clone(),
    }
}

fn record_requirement_kind_gaps(
    entry: &OptionSummary,
    classified_kind: ActionKind,
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) {
    if requirement_implies_other_kind(option_requirement(entry), classified_kind) {
        push_diagnostic(
            diagnostics,
            entry.index,
            None,
            None,
            LegalActionDiagnosticReason::FutureActionKindAmbiguous,
        );
    }
    record_target_requirement_kind_gaps(
        entry.index,
        None,
        &entry.target_entries,
        classified_kind,
        diagnostics,
    );
    for sub in &entry.sub_options {
        if requirement_implies_other_kind(option_requirement(sub), classified_kind) {
            push_diagnostic(
                diagnostics,
                entry.index,
                Some(sub.index),
                None,
                LegalActionDiagnosticReason::FutureActionKindAmbiguous,
            );
        }
        record_target_requirement_kind_gaps(
            entry.index,
            Some(sub.index),
            &sub.target_entries,
            classified_kind,
            diagnostics,
        );
    }
}

fn record_target_requirement_kind_gaps(
    option_index: u32,
    sub_option_index: Option<u32>,
    targets: &[OptionTargetSummary],
    classified_kind: ActionKind,
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) {
    for target in targets {
        let requirement = target.raw_error.as_deref().or(target.error.as_deref());
        if requirement_implies_other_kind(requirement, classified_kind) {
            push_diagnostic(
                diagnostics,
                option_index,
                sub_option_index,
                Some(target.index),
                LegalActionDiagnosticReason::FutureActionKindAmbiguous,
            );
        }
    }
}

fn option_requirement(option: &OptionSummary) -> Option<&str> {
    option.raw_error.as_deref().or(option.error.as_deref())
}

fn requirement_implies_other_kind(requirement: Option<&str>, classified_kind: ActionKind) -> bool {
    let expected = match requirement.unwrap_or_default() {
        "REQ_NOT_MINION_JUST_PLAYED" | "REQ_ATTACK_GREATER_THAN_0" | "REQ_TARGET_TAUNTER" => {
            Some(ActionKind::Attack)
        }
        "REQ_NOT_EXHAUSTED_HERO_POWER" => Some(ActionKind::UseHeroPower),
        "REQ_NOT_EXHAUSTED_ACTIVATE" => Some(ActionKind::ActivateLocation),
        _ => None,
    };
    expected.is_some_and(|expected| expected != classified_kind)
}

fn classify_action<'a>(
    state: &'a ObservedGameState,
    friendly: PlayerId,
    input: ClassificationInput<'_>,
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) -> Option<ClassifiedSource<'a>> {
    if input.option_type.eq_ignore_ascii_case("END_TURN") {
        return Some(ClassifiedSource {
            kind: ActionKind::EndTurn,
            source: None,
            source_state: None,
        });
    }

    let Some(source) = input.source else {
        push_diagnostic(
            diagnostics,
            input.option_index,
            None,
            None,
            LegalActionDiagnosticReason::MissingSource,
        );
        return None;
    };
    let Some(entity) = state.entity(source) else {
        push_diagnostic(
            diagnostics,
            input.option_index,
            None,
            None,
            LegalActionDiagnosticReason::SourceNotObserved,
        );
        return None;
    };
    if entity.controller != Some(friendly) {
        push_diagnostic(
            diagnostics,
            input.option_index,
            None,
            None,
            LegalActionDiagnosticReason::SourceNotFriendly,
        );
        return None;
    }

    let card_type = entity.card_type();
    let kind = if input.option_type.eq_ignore_ascii_case("ATTACK") {
        match card_type {
            Some(CardType::Hero | CardType::Minion) if entity.zone == Zone::Play => {
                ActionKind::Attack
            }
            None | Some(CardType::Unknown(_)) => {
                push_diagnostic(
                    diagnostics,
                    input.option_index,
                    None,
                    None,
                    LegalActionDiagnosticReason::SourceTypeUnknown,
                );
                return None;
            }
            _ => {
                push_diagnostic(
                    diagnostics,
                    input.option_index,
                    None,
                    None,
                    LegalActionDiagnosticReason::ActionKindUnknown,
                );
                return None;
            }
        }
    } else if input.option_type.eq_ignore_ascii_case("POWER") {
        match (entity.zone, card_type) {
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
            (Zone::Play, Some(CardType::Hero | CardType::Minion))
                if input.has_targets && !input.has_sub_options =>
            {
                // Older logs call ordinary attacks POWER. Requiring explicit
                // server targets and no sub-option avoids guessing that an
                // activated minion ability is an attack.
                ActionKind::Attack
            }
            (Zone::Play, Some(CardType::HeroPower)) => ActionKind::UseHeroPower,
            (Zone::Play, Some(CardType::Location)) => ActionKind::ActivateLocation,
            (_, None | Some(CardType::Unknown(_))) => {
                push_diagnostic(
                    diagnostics,
                    input.option_index,
                    None,
                    None,
                    LegalActionDiagnosticReason::SourceTypeUnknown,
                );
                return None;
            }
            _ => {
                push_diagnostic(
                    diagnostics,
                    input.option_index,
                    None,
                    None,
                    LegalActionDiagnosticReason::ActionKindUnknown,
                );
                return None;
            }
        }
    } else {
        push_diagnostic(
            diagnostics,
            input.option_index,
            None,
            None,
            LegalActionDiagnosticReason::ActionKindUnknown,
        );
        return None;
    };

    Some(ClassifiedSource {
        kind,
        source: Some(source),
        source_state: Some(entity),
    })
}

fn selectable_targets(
    state: &ObservedGameState,
    option_index: u32,
    sub_option_index: Option<u32>,
    target_entries: &[OptionTargetSummary],
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) -> Vec<Option<EntityId>> {
    if target_entries.is_empty() {
        return vec![None];
    }

    let mut targets = Vec::new();
    let mut entries = target_entries.iter().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.index);
    for entry in entries {
        if !entry.availability.is_selectable() {
            continue;
        }
        let Some(entity) = entry.entity else {
            push_diagnostic(
                diagnostics,
                option_index,
                sub_option_index,
                Some(entry.index),
                LegalActionDiagnosticReason::TargetNotObserved,
            );
            continue;
        };
        if state.entity(entity).is_none() {
            push_diagnostic(
                diagnostics,
                option_index,
                sub_option_index,
                Some(entry.index),
                LegalActionDiagnosticReason::TargetNotObserved,
            );
            continue;
        }
        targets.push(Some(entity));
    }
    targets
}

fn legal_positions(
    state: &ObservedGameState,
    friendly: PlayerId,
    option_index: u32,
    sub_option_index: Option<u32>,
    kind: ActionKind,
    source_state: Option<&EntityState>,
    diagnostics: &mut Vec<LegalActionDiagnostic>,
) -> Vec<Option<u8>> {
    let needs_position = kind == ActionKind::PlayCard
        && source_state.is_some_and(|entity| {
            matches!(
                entity.card_type(),
                Some(CardType::Minion | CardType::Location)
            )
        });
    if !needs_position {
        return vec![None];
    }

    match board_position_projection(state, friendly) {
        BoardPositionProjection::Available { occupied } => (1..=occupied + 1)
            .map(|position| Some(position as u8))
            .collect(),
        BoardPositionProjection::Full => {
            push_diagnostic(
                diagnostics,
                option_index,
                sub_option_index,
                None,
                LegalActionDiagnosticReason::BoardFull,
            );
            Vec::new()
        }
        BoardPositionProjection::Incomplete => {
            push_diagnostic(
                diagnostics,
                option_index,
                sub_option_index,
                None,
                LegalActionDiagnosticReason::BoardPositionsIncomplete,
            );
            Vec::new()
        }
    }
}

fn board_position_projection(
    state: &ObservedGameState,
    friendly: PlayerId,
) -> BoardPositionProjection {
    let mut observed_positions = Vec::new();
    for entity in state
        .entities
        .values()
        .filter(|entity| entity.controller == Some(friendly) && entity.zone == Zone::Play)
    {
        match entity.card_type() {
            Some(CardType::Minion | CardType::Location) => {
                let Some(position) = entity.zone_position.filter(|position| *position > 0) else {
                    return BoardPositionProjection::Incomplete;
                };
                observed_positions.push(position);
            }
            None | Some(CardType::Unknown(_))
                if entity.card_id.is_none()
                    && entity.zone_position.is_none_or(|position| position == 0) => {}
            None | Some(CardType::Unknown(_)) => return BoardPositionProjection::Incomplete,
            Some(_) => {}
        }
    }
    observed_positions.sort_unstable();
    if observed_positions
        .iter()
        .enumerate()
        .any(|(index, position)| usize::from(*position) != index + 1)
    {
        return BoardPositionProjection::Incomplete;
    }
    let occupied = observed_positions.len();
    if occupied >= MAX_BOARD_SLOTS {
        return BoardPositionProjection::Full;
    }
    BoardPositionProjection::Available { occupied }
}

fn push_diagnostic(
    diagnostics: &mut Vec<LegalActionDiagnostic>,
    option_index: u32,
    sub_option_index: Option<u32>,
    target_index: Option<u32>,
    reason: LegalActionDiagnosticReason,
) {
    diagnostics.push(LegalActionDiagnostic {
        option_index,
        sub_option_index,
        target_index,
        reason,
    });
}

fn same_template_identity(left: &ActionTemplate, right: &ActionTemplate) -> bool {
    left.options_id == right.options_id
        && left.option_index == right.option_index
        && left.sub_option_index == right.sub_option_index
        && left.target == right.target
        && left.position == right.position
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_log_core::game_tag;
    use hs_observed_state::{OptionAvailability, OptionSummary, OptionsState};
    use hs_types::CardId;
    use serde_json::json;

    fn base_state() -> ObservedGameState {
        let mut state = ObservedGameState::default();
        state.game_id = Some(GameId(7));
        state.friendly_player = Some(PlayerId(1));
        state
    }

    fn entity(id: i32, zone: Zone, card_type: Option<i32>, position: Option<u8>) -> EntityState {
        let mut entity =
            EntityState::new(EntityId(id), Some(CardId::new(format!("CARD_{id}"))), zone);
        entity.controller = Some(PlayerId(1));
        entity.zone_position = position;
        if let Some(card_type) = card_type {
            entity.tags.insert(game_tag::CARDTYPE, card_type);
        }
        entity
    }

    fn one_option(entity: i32, issued_revision: u64) -> OptionsState {
        serde_json::from_value(json!({
            "id": 11,
            "issued_revision": issued_revision,
            "options": [{
                "index": 2,
                "option_type": "POWER",
                "entity": entity,
                "entity_raw": entity.to_string(),
                "error": null,
                "availability": "Allowed",
                "raw_error": "NONE",
                "error_param": "",
                "targets": [],
                "target_entries": [],
                "sub_options": []
            }]
        }))
        .expect("deserialize OptionsState fixture")
    }

    #[test]
    fn minion_positions_count_existing_minions_and_locations() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::MINION), None),
        );
        state.entities.insert(
            EntityId(20),
            entity(20, Zone::Play, Some(game_tag::card_type::MINION), Some(1)),
        );
        state.entities.insert(
            EntityId(21),
            entity(21, Zone::Play, Some(game_tag::card_type::LOCATION), Some(2)),
        );
        state.entities.insert(
            EntityId(22),
            entity(22, Zone::Play, Some(game_tag::card_type::HERO), Some(99)),
        );
        let mut placeholder = EntityState::new(EntityId(23), None, Zone::Play);
        placeholder.controller = Some(PlayerId(1));
        state.entities.insert(EntityId(23), placeholder);
        state.options = Some(one_option(10, 31));

        let (set, templates) = legal_actions_with_templates(&state).expect("legal actions");
        assert_eq!(set.issued_revision, 31);
        assert_eq!(
            set.actions
                .iter()
                .map(|action| action.server_ref.position)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(3)]
        );
        assert!(set.diagnostics.is_empty());
        assert_eq!(
            templates.templates()[0].positions_for(&hs_sim::SimGameState::from_observed(&state)),
            vec![Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn unknown_or_noncontiguous_board_slots_block_position_synthesis() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::MINION), None),
        );
        state
            .entities
            .insert(EntityId(20), entity(20, Zone::Play, None, Some(1)));
        state.options = Some(one_option(10, 32));

        let (set, templates) = legal_actions_with_templates(&state).expect("legal actions");
        assert!(set.actions.is_empty());
        assert_eq!(
            set.diagnostics,
            vec![LegalActionDiagnostic {
                option_index: 2,
                sub_option_index: None,
                target_index: None,
                reason: LegalActionDiagnosticReason::BoardPositionsIncomplete,
            }]
        );
        assert!(
            templates.has_projection_gaps(),
            "an unprojectable Allowed board position prevents exhaustive search"
        );
    }

    #[test]
    fn unknown_source_type_is_diagnostic_and_never_a_candidate() {
        let mut state = base_state();
        state
            .entities
            .insert(EntityId(10), entity(10, Zone::Hand, None, None));
        state.options = Some(one_option(10, 4));

        let set = legal_actions(&state).expect("legal actions");
        assert!(set.actions.is_empty());
        assert_eq!(
            set.diagnostics,
            vec![LegalActionDiagnostic {
                option_index: 2,
                sub_option_index: None,
                target_index: None,
                reason: LegalActionDiagnosticReason::SourceTypeUnknown,
            }]
        );
    }

    #[test]
    fn blocked_opponent_option_is_not_a_friendly_closure_gap() {
        let mut state = base_state();
        let mut opponent = entity(30, Zone::Play, Some(game_tag::card_type::MINION), Some(1));
        opponent.controller = Some(PlayerId(2));
        state.entities.insert(EntityId(30), opponent);
        let mut options = one_option(30, 5);
        options.options[0].availability = OptionAvailability::Blocked;
        options.options[0].raw_error = Some("REQ_YOUR_TURN".into());
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty());
        assert!(legal.diagnostics.is_empty());
        assert!(templates.templates().is_empty());
        assert!(!templates.has_projection_gaps());
    }

    #[test]
    fn unknown_source_controller_remains_a_closure_gap() {
        let mut state = base_state();
        let mut unknown = entity(30, Zone::Play, Some(game_tag::card_type::MINION), Some(1));
        unknown.controller = None;
        state.entities.insert(EntityId(30), unknown);
        let mut options = one_option(30, 6);
        options.options[0].availability = OptionAvailability::Blocked;
        options.options[0].raw_error = Some("REQ_YOUR_TURN".into());
        state.options = Some(options);

        let (_, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(templates.templates().is_empty());
        assert!(templates.has_projection_gaps());
    }

    #[test]
    fn action_identity_rejects_other_game_or_issued_revision() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::SPELL), None),
        );
        state.options = Some(one_option(10, 9));
        let action_ref = legal_actions(&state).unwrap().actions[0].server_ref;

        assert!(is_current_action(&state, GameId(7), 9, &action_ref));
        assert!(!is_current_action(&state, GameId(8), 9, &action_ref));
        assert!(!is_current_action(&state, GameId(7), 8, &action_ref));
    }

    #[test]
    fn attack_requires_a_server_target_and_filters_blocked_targets() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(20),
            entity(20, Zone::Play, Some(game_tag::card_type::MINION), Some(1)),
        );
        state.entities.insert(
            EntityId(30),
            entity(30, Zone::Play, Some(game_tag::card_type::MINION), Some(2)),
        );
        let mut options = one_option(20, 7);
        options.options[0].option_type = "ATTACK".into();
        state.options = Some(options.clone());

        let (missing, missing_templates) = legal_actions_with_templates(&state).unwrap();
        assert!(missing.actions.is_empty());
        assert!(
            missing_templates.has_projection_gaps(),
            "缺失攻击目标必须让搜索知道闭包不完整"
        );
        assert_eq!(
            missing.diagnostics,
            vec![LegalActionDiagnostic {
                option_index: 2,
                sub_option_index: None,
                target_index: None,
                reason: LegalActionDiagnosticReason::MissingTargets,
            }]
        );

        let mut target = OptionTargetSummary::default();
        target.index = 3;
        target.entity = Some(EntityId(30));
        target.availability = OptionAvailability::Blocked;
        target.raw_error = Some("REQ_TARGET_TAUNTER".into());
        options.options[0].target_entries = vec![target.clone()];
        state.options = Some(options.clone());
        let (blocked, blocked_templates) = legal_actions_with_templates(&state).unwrap();
        assert!(blocked.actions.is_empty());
        assert_eq!(blocked_templates.templates().len(), 1);
        assert_eq!(
            blocked_templates.templates()[0].target(),
            Some(EntityId(30)),
            "blocked server target remains a simulated continuation template"
        );
        assert!(blocked_templates.templates()[0].has_only_recheckable_gates());

        target.availability = OptionAvailability::Allowed;
        options.options[0].target_entries = vec![target];
        state.options = Some(options);
        let allowed = legal_actions(&state).unwrap();
        assert_eq!(allowed.actions.len(), 1);
        assert_eq!(allowed.actions[0].server_ref.target, Some(EntityId(30)));
        assert_eq!(allowed.actions[0].kind, ActionKind::Attack);
    }

    #[test]
    fn duplicate_target_indexes_merge_by_server_identity_and_keep_weakest_gate() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(20),
            entity(20, Zone::Play, Some(game_tag::card_type::MINION), Some(1)),
        );
        state.entities.insert(
            EntityId(30),
            entity(30, Zone::Play, Some(game_tag::card_type::MINION), Some(2)),
        );
        state.entities.insert(
            EntityId(40),
            entity(40, Zone::Play, Some(game_tag::card_type::MINION), Some(3)),
        );
        let mut options = one_option(20, 14);
        options.options[0].option_type = "ATTACK".into();

        let target = |index, entity, availability, error: Option<&str>| {
            let mut target = OptionTargetSummary::default();
            target.index = index;
            target.entity = Some(EntityId(entity));
            target.availability = availability;
            target.raw_error = error.map(str::to_owned);
            target
        };
        options.options[0].target_entries = vec![
            target(9, 30, OptionAvailability::Allowed, Some("NONE")),
            target(1, 40, OptionAvailability::Allowed, Some("NONE")),
            target(
                2,
                30,
                OptionAvailability::Blocked,
                Some("REQ_CARD_SPECIFIC_TARGET"),
            ),
        ];
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert_eq!(legal.actions.len(), 2);
        assert_eq!(
            templates
                .templates()
                .iter()
                .map(ActionTemplate::target)
                .collect::<Vec<_>>(),
            vec![Some(EntityId(30)), Some(EntityId(40))],
            "模板顺序必须跟 ServerActionRef target 身份一致，而非 target index"
        );
        assert!(
            !templates.templates()[0].has_only_recheckable_gates(),
            "重复身份的未知 blocker 必须保守合并，不能被 Allowed 副本覆盖"
        );
        assert!(templates.templates()[1].has_only_recheckable_gates());
    }

    #[test]
    fn blocked_option_and_future_board_positions_are_templates_not_roots() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::MINION), None),
        );
        for id in 20..27 {
            state.entities.insert(
                EntityId(id),
                entity(
                    id,
                    Zone::Play,
                    Some(game_tag::card_type::MINION),
                    Some((id - 19) as u8),
                ),
            );
        }
        state.entities.insert(
            EntityId(50),
            entity(50, Zone::Play, Some(game_tag::card_type::HERO), Some(99)),
        );
        let mut options = one_option(10, 12);
        options.options[0].availability = OptionAvailability::Blocked;
        options.options[0].raw_error = Some("REQ_ENOUGH_MANA".into());
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty(), "blocked option cannot be a root");
        assert_eq!(templates.templates().len(), 1);
        let template = &templates.templates()[0];
        assert_eq!(template.position, TemplatePosition::DynamicBoardSlot);
        assert!(
            !template.has_only_recheckable_gates(),
            "动态站位被保留，但缺少完整卡牌出牌条件验证前不能作为模拟后续"
        );

        let full = hs_sim::SimGameState::from_observed(&state);
        assert!(template.positions_for(&full).is_empty());
        let mut opened_state = state;
        opened_state.entities.remove(&EntityId(26));
        let opened = hs_sim::SimGameState::from_observed(&opened_state);
        assert_eq!(
            template.positions_for(&opened),
            (1..=7).map(Some).collect::<Vec<_>>(),
            "Hero 的异常正 zone_position 不能占用 Minion/Location 棋盘格"
        );
    }

    #[test]
    fn attack_only_requirement_on_a_hand_minion_marks_future_kind_gap() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::MINION), None),
        );
        let mut options = one_option(10, 18);
        options.options[0].availability = OptionAvailability::Blocked;
        options.options[0].raw_error = Some("REQ_NOT_MINION_JUST_PLAYED".into());
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty());
        assert_eq!(templates.templates().len(), 1);
        assert_eq!(templates.templates()[0].kind(), ActionKind::PlayCard);
        assert!(
            templates.has_projection_gaps(),
            "当前 Hand 分类不能吞掉打出后可能出现的 Attack 坐标"
        );
    }

    #[test]
    fn card_specific_blocked_target_is_retained_but_not_recheckable() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::SPELL), None),
        );
        state.entities.insert(
            EntityId(30),
            entity(30, Zone::Play, Some(game_tag::card_type::MINION), Some(1)),
        );
        let mut options = one_option(10, 13);
        let mut target = OptionTargetSummary::default();
        target.index = 4;
        target.entity = Some(EntityId(30));
        target.availability = OptionAvailability::Blocked;
        target.raw_error = Some("REQ_FRIENDLY_TARGET".into());
        options.options[0].target_entries = vec![target];
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty());
        assert_eq!(templates.templates().len(), 1);
        assert!(!templates.templates()[0].has_only_recheckable_gates());
    }

    #[test]
    fn blocked_sub_option_is_retained_as_a_non_root_template() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::SPELL), None),
        );
        let mut sub = OptionSummary::default();
        sub.index = 4;
        sub.availability = OptionAvailability::Blocked;
        sub.raw_error = Some("REQ_ENOUGH_MANA".into());
        let mut options = one_option(10, 15);
        options.options[0].sub_options = vec![sub];
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty());
        assert!(legal.diagnostics.is_empty());
        assert_eq!(templates.templates().len(), 1);
        assert_eq!(templates.templates()[0].sub_option_index(), Some(4));
        assert!(!templates.templates()[0].has_only_recheckable_gates());
    }

    #[test]
    fn attack_sub_option_is_rejected_instead_of_losing_its_coordinate() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(20),
            entity(20, Zone::Play, Some(game_tag::card_type::MINION), Some(1)),
        );
        state.entities.insert(
            EntityId(30),
            entity(30, Zone::Play, Some(game_tag::card_type::MINION), Some(2)),
        );
        let mut target = OptionTargetSummary::default();
        target.index = 1;
        target.entity = Some(EntityId(30));
        target.availability = OptionAvailability::Allowed;
        let mut sub = OptionSummary::default();
        sub.index = 4;
        sub.availability = OptionAvailability::Allowed;
        sub.target_entries = vec![target];
        let mut options = one_option(20, 17);
        options.options[0].option_type = "ATTACK".into();
        options.options[0].sub_options = vec![sub];
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty());
        assert_eq!(
            legal.diagnostics,
            vec![LegalActionDiagnostic {
                option_index: 2,
                sub_option_index: Some(4),
                target_index: None,
                reason: LegalActionDiagnosticReason::AttackSubOptionUnsupported,
            }]
        );
        assert!(templates.templates().is_empty());
        assert!(templates.has_projection_gaps());
    }

    #[test]
    fn blocked_nested_sub_option_is_a_private_gap_not_a_public_diagnostic() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::SPELL), None),
        );
        let mut nested = OptionSummary::default();
        nested.index = 9;
        nested.availability = OptionAvailability::Blocked;
        let mut sub = OptionSummary::default();
        sub.index = 4;
        sub.availability = OptionAvailability::Blocked;
        sub.sub_options = vec![nested];
        let mut options = one_option(10, 16);
        options.options[0].sub_options = vec![sub];
        state.options = Some(options);

        let (legal, templates) = legal_actions_with_templates(&state).unwrap();
        assert!(legal.actions.is_empty());
        assert!(legal.diagnostics.is_empty());
        assert!(templates.templates().is_empty());
        assert!(templates.has_projection_gaps());
    }

    #[test]
    fn recursively_nested_sub_options_are_conservatively_rejected() {
        let mut state = base_state();
        state.entities.insert(
            EntityId(10),
            entity(10, Zone::Hand, Some(game_tag::card_type::SPELL), None),
        );
        let mut nested = OptionSummary::default();
        nested.index = 9;
        nested.availability = OptionAvailability::Allowed;
        let mut sub = OptionSummary::default();
        sub.index = 4;
        sub.availability = OptionAvailability::Allowed;
        sub.sub_options = vec![nested];
        let mut options = one_option(10, 8);
        options.options[0].sub_options = vec![sub];
        state.options = Some(options);

        let set = legal_actions(&state).unwrap();
        assert!(set.actions.is_empty());
        assert_eq!(
            set.diagnostics,
            vec![LegalActionDiagnostic {
                option_index: 2,
                sub_option_index: Some(4),
                target_index: None,
                reason: LegalActionDiagnosticReason::NestedSubOptionsUnsupported,
            }]
        );
    }
}
