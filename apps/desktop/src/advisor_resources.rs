//! Owned production resources for one exact-Build Advisor job.
//!
//! This module is the desktop-only IO adapter. It snapshots every value needed
//! by `hs-planner`, then hands the worker owned/`Arc` data without retaining a
//! GPUI `Context`, `Entity`, or global-store borrow.

use std::sync::Arc;

use hs_advisor_api::{SearchConfig, UnavailableReason};
use hs_deckstrings::DeckList;
use hs_effect_ir::{EffectProgram, RuleSet};
use hs_observed_state::{EntityState, ObservedGameState};
use hs_planner::{
    prepare_request, BeamEngine, DeckCardCount, DeckSignature, Engine, PlannerError,
    PlannerRequest, PlannerResources, PlannerResult, PlaybookCatalog, PlaybookError,
    SideboardCardCount,
};
use hs_pools::{ContextKey, PoolContext, PoolError, PoolFilter, PoolSnapshot, PoolSpec};
use hs_sim::{CardLookup, PoolResolver, RuleLookup, SimCardDefinition, SimError};
use hs_types::{BuildId, CardId, FormatType, GameMode, PoolId};

use crate::pool_query::SqliteCardQuery;
use crate::rules_store::RulesStore;

/// Immutable application-wide source data used to prepare per-snapshot jobs.
#[derive(Clone)]
pub struct AdvisorResourceCatalog {
    rules: Arc<RuleSet>,
    pools: Arc<[PoolSpec]>,
    cards: Arc<SqliteCardQuery>,
    playbooks: Arc<PlaybookCatalog>,
}

impl AdvisorResourceCatalog {
    pub fn new(
        rules: Arc<RuleSet>,
        pools: Arc<[PoolSpec]>,
        cards: Arc<SqliteCardQuery>,
        playbooks: Arc<PlaybookCatalog>,
    ) -> Self {
        Self {
            rules,
            pools,
            cards,
            playbooks,
        }
    }

    /// Snapshot the production loader into one application-wide Arc catalog.
    pub fn from_store(store: &RulesStore) -> Result<Arc<Self>, PlannerError> {
        if !store.advisor_resources_ready() {
            return Err(PlannerError::Unavailable(
                UnavailableReason::MissingBuildData,
            ));
        }
        let cards = store.card_query.clone().ok_or(PlannerError::Unavailable(
            UnavailableReason::MissingBuildData,
        ))?;
        let playbooks = store.playbooks.clone().ok_or(PlannerError::Unavailable(
            UnavailableReason::MissingBuildData,
        ))?;
        Ok(Arc::new(Self::new(
            store.rule_set.clone(),
            store.pools.as_arc(),
            cards,
            playbooks,
        )))
    }

    /// Bind all planner adapters to the Build carried by this observed state.
    /// `RulesStore::current_build` is intentionally not an input.
    pub fn for_snapshot(
        &self,
        snapshot: &ObservedGameState,
        deck: Option<&DeckList>,
    ) -> Result<(BeamEngine, PlannerResources), PlannerError> {
        let build = snapshot
            .build_id
            .ok_or(PlannerError::Unavailable(UnavailableReason::MissingBuild))?;
        let has_build = self
            .cards
            .has_build(build)
            .map_err(|error| failed("checking exact card database Build", error))?;
        if !has_build
            || !rules_cover_build(&self.rules, build)
            || !pools_cover_build(&self.pools, build)
        {
            return Err(PlannerError::Unavailable(
                UnavailableReason::MissingBuildData,
            ));
        }

        let signature = deck_signature(&self.cards, build, deck)?;
        self.playbooks
            .select(build, signature.as_ref())
            .map_err(map_playbook_selection_error)?;
        let player_classes = deck_player_classes(&self.cards, build, deck)?;
        let format = deck.and_then(|deck| match deck.format {
            FormatType::Unknown(_) => None,
            known => Some(known),
        });

        let rules = Arc::new(BuildRuleLookup {
            build,
            rules: self.rules.clone(),
        });
        let cards = Arc::new(BuildCardLookup {
            build,
            cards: self.cards.clone(),
        });
        let pools = Arc::new(OwnedPoolResolver {
            build,
            specs: self.pools.clone(),
            cards: self.cards.clone(),
            observed: Arc::new(snapshot.clone()),
            format,
            player_classes,
        });
        let resources = PlannerResources::new(build, rules, cards, pools);
        let engine = BeamEngine::new((*self.playbooks).clone(), signature);
        Ok((engine, resources))
    }

    /// Prepare a complete owned job that can be moved directly to a worker.
    pub fn prepare_job(
        &self,
        snapshot: ObservedGameState,
        deck: Option<DeckList>,
        config: SearchConfig,
    ) -> Result<AdvisorJob, PlannerError> {
        let (engine, resources) = self.for_snapshot(&snapshot, deck.as_ref())?;
        let request = prepare_request(snapshot, resources, config)?;
        Ok(AdvisorJob { engine, request })
    }
}

/// A fully owned synchronous planner invocation.
pub struct AdvisorJob {
    engine: BeamEngine,
    request: PlannerRequest,
}

impl AdvisorJob {
    pub fn identity(&self) -> &hs_advisor_api::AdviceIdentity {
        self.request.identity()
    }

    pub fn run(self) -> Result<PlannerResult, PlannerError> {
        self.engine.plan(self.request)
    }
}

struct BuildRuleLookup {
    build: BuildId,
    rules: Arc<RuleSet>,
}

impl RuleLookup for BuildRuleLookup {
    fn program_for(&self, card_id: &CardId) -> Option<&EffectProgram> {
        self.rules.select(self.build, card_id)
    }
}

struct BuildCardLookup {
    build: BuildId,
    cards: Arc<SqliteCardQuery>,
}

impl CardLookup for BuildCardLookup {
    fn sim_card(&self, build: BuildId, card_id: &CardId) -> Option<SimCardDefinition> {
        (build == self.build)
            .then(|| self.cards.sim_card(build, card_id))
            .flatten()
    }
}

/// Pool adapter whose entire context is an owned snapshot.
struct OwnedPoolResolver {
    build: BuildId,
    specs: Arc<[PoolSpec]>,
    cards: Arc<SqliteCardQuery>,
    observed: Arc<ObservedGameState>,
    format: Option<FormatType>,
    player_classes: Option<Vec<String>>,
}

impl PoolResolver for OwnedPoolResolver {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        let spec = select_pool(&self.specs, self.build, pool_id).map_err(SimError::Pool)?;
        let source = unique_source_entity(&self.observed, &spec.source_card_id);
        let source_entity = source.map(|entity| entity.entity_id);
        let current_cost = source.and_then(EntityState::observed_cost);

        validate_pool_context(
            spec,
            self.format,
            self.player_classes.as_deref(),
            source_entity,
            current_cost,
        )
        .map_err(SimError::Pool)?;

        let context = PoolContext {
            build_id: self.build,
            // ObservedGameState does not yet carry GameType. Specs that need
            // it are rejected above; the placeholder is therefore irrelevant.
            game_mode: GameMode::Unknown(0),
            format: self.format.unwrap_or(FormatType::Unknown(0)),
            player_class: self.player_classes.clone().unwrap_or_default(),
            original_class: Vec::new(),
            source_card: spec.source_card_id.clone(),
            source_entity,
            current_cost,
            state_revision: self.observed.revision,
        };
        hs_pools::resolve(spec, &context, self.cards.as_ref(), Some(&self.observed))
            .map_err(SimError::Pool)
    }
}

fn rules_cover_build(rules: &RuleSet, build: BuildId) -> bool {
    rules
        .iter()
        .flat_map(|(_, programs)| programs)
        .any(|program| program.build_range.contains(build))
}

fn pools_cover_build(pools: &[PoolSpec], build: BuildId) -> bool {
    pools.iter().any(|spec| spec.build_range.contains(build))
}

fn select_pool<'a>(
    specs: &'a [PoolSpec],
    build: BuildId,
    pool_id: &PoolId,
) -> Result<&'a PoolSpec, PoolError> {
    let mut matching = specs
        .iter()
        .filter(|spec| &spec.pool_id == pool_id && spec.build_range.contains(build));
    let Some(selected) = matching.next() else {
        return Err(PoolError::Invalid(format!(
            "Build {} 缺池 {pool_id} 的精确版本",
            build.0
        )));
    };
    if matching.next().is_some() {
        return Err(PoolError::Invalid(format!(
            "Build {} 的池 {pool_id} 存在重叠版本",
            build.0
        )));
    }
    Ok(selected)
}

fn validate_pool_context(
    spec: &PoolSpec,
    format: Option<FormatType>,
    player_classes: Option<&[String]>,
    source_entity: Option<hs_types::EntityId>,
    current_cost: Option<i32>,
) -> Result<(), PoolError> {
    let needs_format = spec.context_inputs.contains(&ContextKey::Format)
        || spec
            .filters
            .iter()
            .any(|filter| matches!(filter, PoolFilter::FormatLegal));
    let needs_class = spec.context_inputs.contains(&ContextKey::PlayerClass)
        || spec
            .filters
            .iter()
            .any(|filter| matches!(filter, PoolFilter::ClassMatches { .. }));
    if spec.context_inputs.contains(&ContextKey::GameMode) {
        return Err(missing_context(spec, "game mode"));
    }
    if needs_format && format.is_none() {
        return Err(missing_context(spec, "format"));
    }
    if needs_class && player_classes.is_none_or(<[String]>::is_empty) {
        return Err(missing_context(spec, "player class"));
    }
    if spec.context_inputs.contains(&ContextKey::OriginalClass) {
        return Err(missing_context(spec, "original class"));
    }
    if spec.context_inputs.contains(&ContextKey::SourceEntity) && source_entity.is_none() {
        return Err(missing_context(spec, "unique source entity"));
    }
    if spec.context_inputs.contains(&ContextKey::CurrentCost) && current_cost.is_none() {
        return Err(missing_context(spec, "observed current cost"));
    }
    Ok(())
}

fn missing_context(spec: &PoolSpec, field: &str) -> PoolError {
    PoolError::Invalid(format!("池 {} 缺少精确 {field} 上下文", spec.pool_id))
}

fn unique_source_entity<'a>(
    state: &'a ObservedGameState,
    source_card: &CardId,
) -> Option<&'a EntityState> {
    let mut matching = state
        .entities
        .values()
        .filter(|entity| entity.card_id.as_ref() == Some(source_card))
        .filter(|entity| {
            state
                .friendly_player
                .is_none_or(|friendly| entity.controller == Some(friendly))
        });
    let source = matching.next()?;
    matching.next().is_none().then_some(source)
}

fn deck_signature(
    cards: &SqliteCardQuery,
    build: BuildId,
    deck: Option<&DeckList>,
) -> Result<Option<DeckSignature>, PlannerError> {
    let Some(deck) = deck else {
        return Ok(None);
    };
    let mut main = Vec::with_capacity(deck.cards.len());
    for &(dbf_id, count) in &deck.cards {
        let Some(card_id) = cards
            .card_id_by_dbf(build, dbf_id)
            .map_err(|error| failed("mapping main-deck DBF id", error))?
        else {
            return Ok(None);
        };
        main.push(DeckCardCount {
            card_id,
            count: u16::from(count),
        });
    }

    let sideboard_capacity = deck.sideboards.iter().map(|(_, cards)| cards.len()).sum();
    let mut sideboard = Vec::with_capacity(sideboard_capacity);
    for (owner_dbf, entries) in &deck.sideboards {
        let Some(owner_card_id) = cards
            .card_id_by_dbf(build, *owner_dbf)
            .map_err(|error| failed("mapping sideboard owner DBF id", error))?
        else {
            return Ok(None);
        };
        for &(dbf_id, count) in entries {
            let Some(card_id) = cards
                .card_id_by_dbf(build, dbf_id)
                .map_err(|error| failed("mapping sideboard DBF id", error))?
            else {
                return Ok(None);
            };
            sideboard.push(SideboardCardCount {
                owner_card_id: owner_card_id.clone(),
                card_id,
                count: u16::from(count),
            });
        }
    }
    Ok(DeckSignature::new(main, sideboard).ok())
}

fn deck_player_classes(
    cards: &SqliteCardQuery,
    build: BuildId,
    deck: Option<&DeckList>,
) -> Result<Option<Vec<String>>, PlannerError> {
    let Some(deck) = deck else {
        return Ok(None);
    };
    let mut classes = Vec::with_capacity(deck.heroes.len());
    for &hero in &deck.heroes {
        let Some(class) = cards
            .card_class_by_dbf(build, hero)
            .map_err(|error| failed("mapping exact-Build hero class", error))?
        else {
            return Ok(None);
        };
        classes.push(class);
    }
    classes.sort();
    classes.dedup();
    Ok((!classes.is_empty()).then_some(classes))
}

fn failed(context: &str, error: impl std::fmt::Display) -> PlannerError {
    PlannerError::Failed {
        message: format!("{context}: {error}"),
    }
}

fn map_playbook_selection_error(error: PlaybookError) -> PlannerError {
    match error {
        PlaybookError::GenericDoesNotCoverBuild { .. } => {
            PlannerError::Unavailable(UnavailableReason::MissingBuildData)
        }
        error => failed("selecting exact-Build Playbook", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_card_db::{CardRecord, GameBuildRecord, SqliteCardRepository};
    use hs_log_core::game_tag;
    use hs_types::{EntityId, PlayerId, Zone};

    const DATABASE_BUILD: BuildId = BuildId(249_896);

    fn catalog_with_database_current_build() -> AdvisorResourceCatalog {
        let mut repository =
            SqliteCardRepository::open_in_memory().expect("open in-memory card database");
        repository
            .import_build(
                &GameBuildRecord {
                    build_id: DATABASE_BUILD,
                    version_text: Some("test".into()),
                    source_manifest: "memory://advisor-resources".into(),
                    source_hash: "test".into(),
                },
                &[],
            )
            .expect("seed database Build");
        assert_eq!(
            repository.current_build().expect("read current Build"),
            Some(DATABASE_BUILD)
        );

        let playbooks = PlaybookCatalog::from_sources([(
            "generic.yaml",
            include_str!("../../../rules/playbooks/generic.yaml"),
        )])
        .expect("load generic Playbook");
        AdvisorResourceCatalog::new(
            Arc::new(RuleSet::default()),
            Arc::from(Vec::<PoolSpec>::new()),
            Arc::new(SqliteCardQuery::new(repository)),
            Arc::new(playbooks),
        )
    }

    fn pool_card(
        card_id: &str,
        dbf_id: i64,
        card_set: &str,
        card_class: &str,
        cost: i32,
    ) -> CardRecord {
        CardRecord {
            build_id: DATABASE_BUILD,
            card_id: CardId::new(card_id),
            dbf_id: Some(dbf_id),
            card_type: Some("SPELL".into()),
            card_set: Some(card_set.into()),
            rarity: None,
            card_class_json: Some(format!(r#"["{card_class}"]"#)),
            cost: Some(cost),
            attack: None,
            health: None,
            collectible: true,
            raw_json: "{}".into(),
            localizations: Vec::new(),
        }
    }

    fn pool_query(records: &[CardRecord]) -> Arc<SqliteCardQuery> {
        let mut repository =
            SqliteCardRepository::open_in_memory().expect("open in-memory card database");
        repository
            .import_build(
                &GameBuildRecord {
                    build_id: DATABASE_BUILD,
                    version_text: Some("test".into()),
                    source_manifest: "memory://owned-pool-resolver".into(),
                    source_hash: "test".into(),
                },
                records,
            )
            .expect("seed pool cards");
        Arc::new(SqliteCardQuery::new(repository))
    }

    #[test]
    fn production_resources_and_jobs_are_send_sync_owned_values() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<AdvisorResourceCatalog>();
        assert_send_sync::<AdvisorJob>();
        assert_send_sync::<BuildRuleLookup>();
        assert_send_sync::<BuildCardLookup>();
        assert_send_sync::<OwnedPoolResolver>();
    }

    #[test]
    fn missing_observed_build_is_reported_before_resource_lookup() {
        let catalog = catalog_with_database_current_build();
        let snapshot = ObservedGameState::default();

        assert!(matches!(
            catalog.for_snapshot(&snapshot, None),
            Err(PlannerError::Unavailable(UnavailableReason::MissingBuild))
        ));
    }

    #[test]
    fn observed_build_never_falls_back_to_database_current_build() {
        let catalog = catalog_with_database_current_build();
        let mut snapshot = ObservedGameState::default();
        snapshot.build_id = Some(BuildId(DATABASE_BUILD.0 + 1));

        assert!(matches!(
            catalog.for_snapshot(&snapshot, None),
            Err(PlannerError::Unavailable(
                UnavailableReason::MissingBuildData
            ))
        ));
    }

    #[test]
    fn missing_playbook_build_is_unavailable_while_invalid_catalog_errors_fail() {
        assert_eq!(
            map_playbook_selection_error(PlaybookError::GenericDoesNotCoverBuild {
                build: DATABASE_BUILD,
            }),
            PlannerError::Unavailable(UnavailableReason::MissingBuildData)
        );

        for error in [
            PlaybookError::MissingGeneric,
            PlaybookError::AmbiguousExactMatch {
                left: "left".into(),
                right: "right".into(),
            },
        ] {
            assert!(matches!(
                map_playbook_selection_error(error),
                PlannerError::Failed { .. }
            ));
        }
    }

    #[test]
    fn owned_pool_resolver_executes_with_exact_format_class_source_and_cost() {
        let spec = hs_pools::parse_spec(
            r#"
pool_id: advisor.contextual
source_card_id: SOURCE
build_range:
  min: 249896
  max: 249896
kind: discover
filters:
  - format_legal
  - !class_matches
    include_neutral: false
  - !cost_between
    min: 2
    max: 2
  - !collectible true
  - exclude_source
context_inputs: [build, format, player_class, source_card, source_entity, current_cost]
selection:
  picks: 1
  distinct: true
weight_rule: uniform
status: verified
"#,
        )
        .expect("valid contextual pool");
        let cards = pool_query(&[
            pool_card("SOURCE", 1, "CORE", "HUNTER", 2),
            pool_card("MATCH", 2, "CORE", "HUNTER", 2),
            pool_card("WRONG_CLASS", 3, "CORE", "MAGE", 2),
            pool_card("WRONG_SET", 4, "EXPERT1", "HUNTER", 2),
            pool_card("WRONG_COST", 5, "CORE", "HUNTER", 3),
        ]);

        let friendly = PlayerId(1);
        let source_entity = EntityId(42);
        let mut source = EntityState::new(source_entity, Some(CardId::new("SOURCE")), Zone::Hand);
        source.controller = Some(friendly);
        source.tags.insert(game_tag::COST, 2);
        let mut observed = ObservedGameState::default();
        observed.build_id = Some(DATABASE_BUILD);
        observed.friendly_player = Some(friendly);
        observed.revision = 7;
        observed.entities.insert(source_entity, source);

        let resolver = OwnedPoolResolver {
            build: DATABASE_BUILD,
            specs: Arc::from(vec![spec]),
            cards,
            observed: Arc::new(observed),
            format: Some(FormatType::Standard),
            player_classes: Some(vec!["HUNTER".into()]),
        };
        let snapshot = resolver
            .snapshot(&PoolId::new("advisor.contextual"))
            .expect("owned production resolver should resolve exact context");

        assert_eq!(snapshot.build_id, DATABASE_BUILD);
        assert_eq!(snapshot.pool_id, PoolId::new("advisor.contextual"));
        assert_eq!(snapshot.candidates.len(), 1);
        assert_eq!(snapshot.candidates[0].card_id, CardId::new("MATCH"));
        assert!(snapshot.exact);
    }
}
