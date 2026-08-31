//! 真实 Advisor 数据通路的 headless 自检。
//!
//! 复用桌面生产加载器、精确 Build 资源适配器、BeamEngine 和展示投影；不启动 GPUI。
//! 用法：`cargo run -p desktop --example advisor_selfcheck`

#[path = "../src/advisor_resources.rs"]
mod advisor_resources;
#[path = "../src/pool_query.rs"]
mod pool_query;
#[path = "../src/rules_store.rs"]
mod rules_store;

// `advice_presenter` 只需要这份公开状态摘要。生产枚举当前与 GPUI AdviceSession
// 同文件，因此 headless example 提供等形输入，展示转换本身仍直接编译生产源码。
mod advisor_session {
    use hs_advisor_api::UnavailableReason;

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum AdviceSessionStatus {
        Unavailable(UnavailableReason),
        Debouncing,
        Searching,
        Ready,
        Failed { message: String },
    }
}

#[path = "../src/advice_presenter.rs"]
mod advice_presenter;

use advice_presenter::{AdviceDisplayState, LiveAdviceModel, OverlayAdviceModel};
use advisor_session::AdviceSessionStatus;
use chrono::{TimeZone as _, Utc};
use hs_advisor_api::{ActionOrigin, SearchConfig, UnavailableReason};
use hs_deckstrings::DeckList;
use hs_log_core::{
    game_tag, DomainEvent, OptionAvailability, OptionSummary, OptionTargetSummary, TagKey,
};
use hs_observed_state::{reduce, EntityState, ObservedGameState};
use hs_planner::{is_current_action, PlannerError, PlannerResult};
use hs_types::{BuildId, CardId, EntityId, FormatType, GameId, GamePhase, PlayerId, Zone};
use rules_store::RulesStore;

const BUILD: BuildId = BuildId(249_896);
const MISSING_BUILD_DATA: BuildId = BuildId(999_999_999);
const OPTIONS_ID: u64 = 249_896_001;
const FRIENDLY: PlayerId = PlayerId(1);
const OPPONENT: PlayerId = PlayerId(2);
const FRIENDLY_HERO: EntityId = EntityId(4);
const OPPONENT_HERO: EntityId = EntityId(5);

/// 用户提供并归档于 Trellis research 的标准任务猎；必须走 decode 才进入生产资源层。
const QUEST_HUNTER_DECKSTRING: &str =
    "AAECAda+BQiYoATNngal/AaSgweolweDwAeu2Aen9wcLqZ8E4PgFr5IHzJYH3pYH15cH/ZsH+rAH19oH29oHnIIIAAED3/4GrtgHg4QHrtgHupUHrtgHAAA=";

fn main() {
    let mut failures = Vec::new();

    println!("== Advisor production selfcheck ==");
    let store = RulesStore::load_default();
    println!("rules dir       : {}", store.rules_dir.display());
    println!("card db build   : {:?}", store.current_build);
    println!(
        "rules / pools   : {} / {}",
        store.rule_set.len(),
        store.pools.len()
    );
    println!("playbooks       : {}", store.playbooks.is_some());
    for error in &store.errors {
        failures.push(format!("production resource load failed: {error}"));
    }

    let deck = match hs_deckstrings::decode(QUEST_HUNTER_DECKSTRING) {
        Ok(deck) => {
            check_deck(&deck, &mut failures);
            Some(deck)
        }
        Err(error) => {
            failures.push(format!("quest hunter deckstring decode failed: {error}"));
            None
        }
    };
    let catalog = match advisor_resources::AdvisorResourceCatalog::from_store(&store) {
        Ok(catalog) => Some(catalog),
        Err(error) => {
            failures.push(format!("production Advisor catalog unavailable: {error}"));
            None
        }
    };

    if let (Some(deck), Some(catalog)) = (deck.as_ref(), catalog.as_ref()) {
        let state = quest_hunter_state();
        let config = fixed_config();
        let first = run_job(catalog, &state, deck, config, "first", &mut failures);
        let second = run_job(catalog, &state, deck, config, "repeat", &mut failures);

        if let (Some(first), Some(second)) = (first, second) {
            check_determinism(&first, &second, &mut failures);
            check_result(&state, &first, &mut failures);
            check_presenters(&state, &first, &mut failures);
        }

        check_negative_builds(catalog, deck, &state, config, &mut failures);
        check_unreliable_roots(catalog, deck, &state, config, &mut failures);
    }

    println!("\n== Result ==");
    if failures.is_empty() {
        println!("PASS: exact Build resources, real Beam planning, deterministic payload,");
        println!("      server Options back-reference, simulated follow-ups and UI trimming.");
    } else {
        for failure in &failures {
            println!("FAIL: {failure}");
        }
        std::process::exit(1);
    }
}

fn check_deck(deck: &DeckList, failures: &mut Vec<String>) {
    let main_count: u64 = deck.cards.iter().map(|(_, count)| u64::from(*count)).sum();
    let sideboard_count: u64 = deck
        .sideboards
        .iter()
        .flat_map(|(_, cards)| cards)
        .map(|(_, count)| u64::from(*count))
        .sum();
    println!(
        "decoded deck     : format={:?}, heroes={}, main={main_count}, sideboard={sideboard_count}",
        deck.format,
        deck.heroes.len()
    );
    if deck.format != FormatType::Standard {
        failures.push(format!(
            "quest hunter format is {:?}, expected Standard",
            deck.format
        ));
    }
    if deck.heroes.len() != 1 {
        failures.push(format!(
            "quest hunter has {} heroes, expected 1",
            deck.heroes.len()
        ));
    }
    if main_count != 30 {
        failures.push(format!(
            "quest hunter main deck has {main_count} cards, expected 30"
        ));
    }
    if sideboard_count != 3 {
        failures.push(format!(
            "quest hunter sideboard has {sideboard_count} cards, expected 3"
        ));
    }
}

fn fixed_config() -> SearchConfig {
    SearchConfig {
        seed: 249_896_002_026,
        sim_branch_budget: 16,
        max_depth: 2,
        beam_width: 8,
        node_budget: 256,
        candidate_budget: 512,
    }
}

fn run_job(
    catalog: &advisor_resources::AdvisorResourceCatalog,
    state: &ObservedGameState,
    deck: &DeckList,
    config: SearchConfig,
    label: &str,
    failures: &mut Vec<String>,
) -> Option<PlannerResult> {
    let job = match catalog.prepare_job(state.clone(), Some(deck.clone()), config) {
        Ok(job) => job,
        Err(error) => {
            failures.push(format!(
                "{label} production job preparation failed: {error}"
            ));
            return None;
        }
    };
    println!(
        "{label:>7} identity : game={} revision={} options={} build={}",
        job.identity().game_id.0,
        job.identity().state_revision,
        job.identity().options_id,
        job.identity().build_id.0,
    );
    match job.run() {
        Ok(result) => Some(result),
        Err(error) => {
            failures.push(format!("{label} real Beam job failed: {error}"));
            None
        }
    }
}

fn check_determinism(first: &PlannerResult, second: &PlannerResult, failures: &mut Vec<String>) {
    if first.identity() != second.identity() {
        failures.push("fixed input produced different AdviceIdentity values".into());
    }
    if first.payload() != second.payload() {
        failures
            .push("fixed input/seed produced different deterministic AdvicePayload values".into());
    } else {
        println!("determinism      : identity + payload exactly equal");
    }
}

fn check_result(state: &ObservedGameState, result: &PlannerResult, failures: &mut Vec<String>) {
    let payload = result.payload();
    let Some(line) = payload.lines.first() else {
        failures.push("real Beam result has no advice lines".into());
        return;
    };
    let Some(options) = state.options.as_ref() else {
        failures.push("positive state unexpectedly has no Options".into());
        return;
    };

    if line.first_action.server_ref.options_id != OPTIONS_ID
        || !is_current_action(
            state,
            result.identity().game_id,
            options.issued_revision,
            &line.first_action.server_ref,
        )
    {
        failures.push("top action does not back-reference the current server Options".into());
    }
    if line
        .follow_up
        .iter()
        .any(|action| action.origin() != ActionOrigin::Simulated)
    {
        failures.push("a follow-up action is not explicitly marked simulated".into());
    }
    if !payload.lines.iter().any(|candidate| {
        candidate.first_action.kind == hs_advisor_api::ActionKind::ActivateLocation
    }) {
        failures.push("real Beam result omitted the JAIL_877 ActivateLocation candidate".into());
    }
    if !payload
        .rule_coverage
        .items()
        .iter()
        .any(|item| item.card_id.as_str() == "JAIL_877")
    {
        failures.push("real Beam coverage omitted the exercised JAIL_877 rule".into());
    }

    println!(
        "top action       : {:?} {:?}",
        line.first_action.kind, line.first_action.server_ref
    );
    println!(
        "follow-ups       : {} (all simulated)",
        line.follow_up.len()
    );
    println!("lethal           : {:?}", line.lethal);
    println!("score            : {} milli", line.score.total_milli());
    println!("score components : {}", line.score.components().len());
    println!(
        "confidence       : {:.0}%",
        payload.confidence.confidence().value() * 100.0
    );
    println!(
        "rule coverage    : {}/{} ({:.0}%), verified={}, partial={}, unknown={}, unsupported={}, runtime_unknown={}",
        payload.rule_coverage.covered_count(),
        payload.rule_coverage.total_count(),
        payload.rule_coverage.coverage_ratio().value() * 100.0,
        payload.rule_coverage.verified_count(),
        payload.rule_coverage.status_count(hs_types::RuleStatus::Partial),
        payload.rule_coverage.status_count(hs_types::RuleStatus::Unknown),
        payload.rule_coverage.missing_count(),
        payload.rule_coverage.runtime_unknown_count(),
    );
    println!(
        "search           : reliable {}/{} ({:.0}%)",
        payload.search_completeness.reliably_compared(),
        payload.search_completeness.total(),
        payload.search_completeness.ratio().value() * 100.0,
    );
    for warning in &payload.warnings {
        println!("warning          : {} — {}", warning.code, warning.message);
    }
}

fn check_presenters(state: &ObservedGameState, result: &PlannerResult, failures: &mut Vec<String>) {
    let generated_at = Utc
        .with_ymd_and_hms(2026, 8, 30, 0, 0, 0)
        .single()
        .expect("fixed UTC timestamp");
    let advice = result.clone().into_advice_result(generated_at);
    let live = LiveAdviceModel::from_current(&AdviceSessionStatus::Ready, state, Some(&advice));
    let overlay =
        OverlayAdviceModel::from_current(&AdviceSessionStatus::Ready, state, Some(&advice));

    if live.state != AdviceDisplayState::Ready || overlay.state != AdviceDisplayState::Ready {
        failures.push("production presenters did not expose a current Ready result".into());
    }
    if live.alternatives.len() > 3 {
        failures.push(format!(
            "Live presenter exposed {} alternatives, expected at most 3",
            live.alternatives.len()
        ));
    }
    let Some(primary) = live.primary.as_ref() else {
        failures.push("Live presenter omitted the real top action".into());
        return;
    };
    if overlay.action.as_ref() != Some(&primary.action) {
        failures.push("Live and Overlay presenters disagree on the top action".into());
    }
    if primary
        .previews
        .iter()
        .any(|preview| !preview.starts_with("沙盘预演"))
    {
        failures.push("Live presenter omitted a simulated-preview label".into());
    }
    println!("Live action      : {}", primary.action);
    println!("Live alternatives: {}", live.alternatives.len());
    println!(
        "Overlay          : {} | {} | {}",
        overlay.action.as_deref().unwrap_or("<none>"),
        overlay.confidence.as_deref().unwrap_or("<none>"),
        overlay.risk.as_deref().unwrap_or("<none>"),
    );

    for inactive in [
        AdviceSessionStatus::Unavailable(UnavailableReason::NoOptions),
        AdviceSessionStatus::Debouncing,
        AdviceSessionStatus::Searching,
        AdviceSessionStatus::Failed {
            message: "selfcheck synthetic failure".into(),
        },
    ] {
        let inactive_overlay = OverlayAdviceModel::from_current(&inactive, state, Some(&advice));
        if inactive_overlay.action.is_some() {
            failures.push(format!(
                "Overlay retained an action while status was {inactive:?}"
            ));
        }
    }
}

fn check_negative_builds(
    catalog: &advisor_resources::AdvisorResourceCatalog,
    deck: &DeckList,
    positive: &ObservedGameState,
    config: SearchConfig,
    failures: &mut Vec<String>,
) {
    let mut missing = positive.clone();
    missing.build_id = None;
    expect_unavailable(
        catalog.prepare_job(missing, Some(deck.clone()), config),
        UnavailableReason::MissingBuild,
        "missing observed Build",
        failures,
    );

    let mut absent = positive.clone();
    absent.build_id = Some(MISSING_BUILD_DATA);
    expect_unavailable(
        catalog.prepare_job(absent, Some(deck.clone()), config),
        UnavailableReason::MissingBuildData,
        "nonexistent exact Build",
        failures,
    );
}

fn check_unreliable_roots(
    catalog: &advisor_resources::AdvisorResourceCatalog,
    deck: &DeckList,
    positive: &ObservedGameState,
    config: SearchConfig,
    failures: &mut Vec<String>,
) {
    let mut missing_rule = positive.clone();
    let unknown_spell = EntityId(70);
    missing_rule.entities.insert(
        unknown_spell,
        hand_spell(unknown_spell, "ADVISOR_SELFCHECK_UNKNOWN", FRIENDLY, 0),
    );
    missing_rule = reduce(
        &missing_rule,
        &DomainEvent::OptionsUpdated {
            id: OPTIONS_ID + 1,
            options: vec![play_option(1, unknown_spell)],
        },
    )
    .expect("unknown-rule Options are valid");
    let missing_rule_result = catalog
        .prepare_job(missing_rule, Some(deck.clone()), config)
        .and_then(advisor_resources::AdvisorJob::run);
    expect_unavailable(
        missing_rule_result,
        UnavailableReason::NoReliableActions,
        "unknown rule root",
        failures,
    );

    let mut unsupported = positive.clone();
    let mut unknown_option = OptionSummary::default();
    unknown_option.index = 1;
    unknown_option.option_type = "ADVISOR_SELFCHECK_UNSUPPORTED".into();
    unknown_option.entity = Some(EntityId(60));
    unknown_option.availability = OptionAvailability::Allowed;
    unsupported = reduce(
        &unsupported,
        &DomainEvent::OptionsUpdated {
            id: OPTIONS_ID + 2,
            options: vec![unknown_option],
        },
    )
    .expect("unsupported Options are valid");
    expect_unavailable(
        catalog.prepare_job(unsupported, Some(deck.clone()), config),
        UnavailableReason::NoReliableActions,
        "unsupported action root",
        failures,
    );

    let status = AdviceSessionStatus::Unavailable(UnavailableReason::NoReliableActions);
    let live = LiveAdviceModel::from_current(&status, positive, None);
    let overlay = OverlayAdviceModel::from_current(&status, positive, None);
    if live.state != AdviceDisplayState::Unavailable
        || overlay.state != AdviceDisplayState::Unavailable
        || live.primary.is_some()
        || overlay.action.is_some()
        || live.status.is_empty()
        || overlay.status.is_empty()
    {
        failures.push(
            "NoReliableActions presenters did not expose an honest action-free unavailable state"
                .into(),
        );
    } else {
        println!(
            "negative presenter fallback       : {} | {}",
            live.status, overlay.status
        );
    }
}

fn expect_unavailable<T>(
    actual: Result<T, PlannerError>,
    expected: UnavailableReason,
    label: &str,
    failures: &mut Vec<String>,
) {
    match actual {
        Err(PlannerError::Unavailable(reason)) if reason == expected => {
            println!("negative {label:<24}: Unavailable({reason:?})");
        }
        Err(error) => failures.push(format!(
            "{label} returned {error}, expected Unavailable({expected:?})"
        )),
        Ok(_) => failures.push(format!(
            "{label} unexpectedly prepared a job; expected Unavailable({expected:?})"
        )),
    }
}

fn quest_hunter_state() -> ObservedGameState {
    let mut state = ObservedGameState::default();
    state.build_id = Some(BUILD);
    state.game_id = Some(GameId(9_001));
    state.phase = GamePhase::Playing;
    state.turn = 7;
    state.current_player = Some(FRIENDLY);
    state.friendly_player = Some(FRIENDLY);

    add_player(&mut state, FRIENDLY, EntityId(1), FRIENDLY_HERO);
    add_player(&mut state, OPPONENT, EntityId(2), OPPONENT_HERO);
    state
        .entities
        .insert(FRIENDLY_HERO, hero(FRIENDLY_HERO, "HERO_05", FRIENDLY, 30));
    state
        .entities
        .insert(OPPONENT_HERO, hero(OPPONENT_HERO, "HERO_08", OPPONENT, 24));
    state.entities.insert(
        EntityId(40),
        minion(EntityId(40), "TIME_053", FRIENDLY, 7, 2, 1),
    );
    state.entities.insert(
        EntityId(41),
        minion(EntityId(41), "CORE_EX1_014", FRIENDLY, 5, 6, 2),
    );
    state.entities.insert(
        EntityId(60),
        active_location(EntityId(60), "JAIL_877", FRIENDLY, 3),
    );

    let mut options = vec![
        attack_option(1, EntityId(40), OPPONENT_HERO),
        attack_option(2, EntityId(41), OPPONENT_HERO),
        play_option(3, EntityId(60)),
    ];
    let mut end_turn = OptionSummary::default();
    end_turn.index = 4;
    end_turn.option_type = "END_TURN".into();
    end_turn.availability = OptionAvailability::Allowed;
    options.push(end_turn);

    reduce(
        &state,
        &DomainEvent::OptionsUpdated {
            id: OPTIONS_ID,
            options,
        },
    )
    .expect("fixed Options are valid")
}

fn add_player(
    state: &mut ObservedGameState,
    player: PlayerId,
    entity_id: EntityId,
    hero_id: EntityId,
) {
    let mut entity = EntityState::new(entity_id, None, Zone::Play);
    entity.controller = Some(player);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::PLAYER);
    state.entities.insert(entity_id, entity);
    apply_tag(state, entity_id, TagKey::PLAYER_ID, player.0);
    apply_tag(state, entity_id, TagKey(game_tag::RESOURCES), 6);
    apply_tag(state, entity_id, TagKey(game_tag::RESOURCES_USED), 0);
    apply_tag(state, entity_id, TagKey(game_tag::HERO_ENTITY), hero_id.0);
}

fn apply_tag(state: &mut ObservedGameState, entity_id: EntityId, tag: TagKey, value: i32) {
    *state = reduce(
        state,
        &DomainEvent::TagChanged {
            entity_id,
            tag,
            value,
        },
    )
    .expect("fixed tag is valid");
}

fn hero(entity_id: EntityId, card_id: &str, controller: PlayerId, health: i32) -> EntityState {
    let mut entity = EntityState::new(entity_id, Some(CardId::new(card_id)), Zone::Play);
    entity.controller = Some(controller);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::HERO);
    entity.tags.insert(game_tag::HEALTH, health);
    entity.tags.insert(game_tag::DAMAGE, 0);
    entity.tags.insert(game_tag::ATK, 0);
    entity.tags.insert(game_tag::id_by_name("ARMOR"), 0);
    entity
}

fn minion(
    entity_id: EntityId,
    card_id: &str,
    controller: PlayerId,
    attack: i32,
    health: i32,
    position: u8,
) -> EntityState {
    let mut entity = EntityState::new(entity_id, Some(CardId::new(card_id)), Zone::Play);
    entity.controller = Some(controller);
    entity.zone_position = Some(position);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::MINION);
    entity.tags.insert(game_tag::ATK, attack);
    entity.tags.insert(game_tag::HEALTH, health);
    entity.tags.insert(game_tag::DAMAGE, 0);
    entity
}

fn active_location(
    entity_id: EntityId,
    card_id: &str,
    controller: PlayerId,
    position: u8,
) -> EntityState {
    let mut entity = EntityState::new(entity_id, Some(CardId::new(card_id)), Zone::Play);
    entity.controller = Some(controller);
    entity.zone_position = Some(position);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::LOCATION);
    entity.tags.insert(game_tag::HEALTH, 2);
    entity.tags.insert(game_tag::DAMAGE, 0);
    entity.tags.insert(game_tag::id_by_name("DURABILITY"), 2);
    entity
        .tags
        .insert(game_tag::id_by_name("LOCATION_ACTION_COOLDOWN"), 0);
    entity
        .tags
        .insert(game_tag::id_by_name("LOCATION_ACTION_COST"), 0);
    entity
}

fn hand_spell(entity_id: EntityId, card_id: &str, controller: PlayerId, cost: i32) -> EntityState {
    let mut entity = EntityState::new(entity_id, Some(CardId::new(card_id)), Zone::Hand);
    entity.controller = Some(controller);
    entity
        .tags
        .insert(game_tag::CARDTYPE, game_tag::card_type::SPELL);
    entity.tags.insert(game_tag::COST, cost);
    entity
}

fn attack_option(index: u32, source: EntityId, target: EntityId) -> OptionSummary {
    let mut option = OptionSummary::default();
    option.index = index;
    option.option_type = "ATTACK".into();
    option.entity = Some(source);
    option.availability = OptionAvailability::Allowed;
    let mut target_entry = OptionTargetSummary::default();
    target_entry.index = 1;
    target_entry.entity = Some(target);
    target_entry.availability = OptionAvailability::Allowed;
    option.target_entries = vec![target_entry];
    option
}

fn play_option(index: u32, source: EntityId) -> OptionSummary {
    let mut option = OptionSummary::default();
    option.index = index;
    option.option_type = "POWER".into();
    option.entity = Some(source);
    option.availability = OptionAvailability::Allowed;
    option
}
