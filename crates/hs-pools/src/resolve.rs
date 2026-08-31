//! 池求值主流程与卡牌库查询抽象。
//!
//! `CardQuery` 隔离 SQLite:生产实现在 `hs-card-db`(适配器),
//! 测试用内存表——`hs-pools` 自身保持无 IO。

use crate::context::context_hash;
use crate::sets::legal_card_sets;
use crate::snapshot::{PoolTraceEntry, WeightedCard};
use crate::{ContextKey, PoolContext, PoolError, PoolFilter, PoolKind, PoolSnapshot, PoolSpec};
use hs_observed_state::ObservedGameState;
use hs_types::{CardId, DbfId, PlayerId, Zone};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 数据库查询型池的过滤条件(全部可选,None = 不约束;AND 语义)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CardFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collectible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_type: Option<String>,
    /// 职业码集合(任一匹配;如 ["MAGE", "NEUTRAL"])。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rarity: Option<String>,
    /// 卡牌集合码(格式合法性过滤;如标准轮换内的 set 列表)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_sets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_min: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_max: Option<i32>,
}

/// 查询返回的卡牌元数据(池过滤所需的最小投影)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardMeta {
    pub card_id: CardId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dbf_id: Option<DbfId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_set: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rarity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<i32>,
    pub collectible: bool,
}

/// 卡牌库查询抽象(按 build 查询;实现方保证结果按 card_id 升序)。
pub trait CardQuery {
    fn query(
        &self,
        build: hs_types::BuildId,
        filter: &CardFilter,
    ) -> Result<Vec<CardMeta>, PoolError>;
}

/// 把 kind 蕴含的类型约束与 filters 中可下推的条件组装成 [`CardFilter`]。
///
/// [`resolve`] 的初始查询只用 kind 约束,其余条件在内存逐级应用,
/// 以便 Trace 每级 `removed`/`remaining` 数字准确。本函数供适配器与测试
/// 复用同一套下推映射(FormatLegal → `card_sets`,ClassMatches → `card_classes`)。
pub fn assemble_card_filter(spec: &PoolSpec, ctx: &PoolContext) -> CardFilter {
    let mut filter = kind_card_filter(spec.kind);
    for item in &spec.filters {
        match item {
            PoolFilter::CardType(card_type) => {
                filter.card_type = Some(card_type.clone());
            }
            PoolFilter::Collectible(flag) => {
                filter.collectible = Some(*flag);
            }
            PoolFilter::Rarity(rarity) => {
                filter.rarity = Some(rarity.clone());
            }
            PoolFilter::CostBetween { min, max } => {
                filter.cost_min = *min;
                filter.cost_max = *max;
            }
            PoolFilter::CardClass(class) => {
                if !filter.card_classes.iter().any(|c| c == class) {
                    filter.card_classes.push(class.clone());
                }
            }
            PoolFilter::ClassMatches { include_neutral } => {
                for class in &ctx.player_class {
                    if !filter.card_classes.iter().any(|c| c == class) {
                        filter.card_classes.push(class.clone());
                    }
                }
                if *include_neutral && !filter.card_classes.iter().any(|c| c == "NEUTRAL") {
                    filter.card_classes.push("NEUTRAL".into());
                }
            }
            PoolFilter::FormatLegal => {
                if let Some(sets) = legal_card_sets(ctx.format) {
                    filter.card_sets = sets.iter().map(|s| (*s).to_string()).collect();
                }
            }
            PoolFilter::Tribe(_) | PoolFilter::ExcludeSource => {}
        }
    }
    filter
}

/// [`CardMeta`] 是否满足 [`CardFilter`](AND)。供内存实现与适配器对照。
pub fn card_matches_filter(card: &CardMeta, filter: &CardFilter) -> bool {
    if let Some(flag) = filter.collectible {
        if card.collectible != flag {
            return false;
        }
    }
    if let Some(card_type) = &filter.card_type {
        if card.card_type.as_deref() != Some(card_type.as_str()) {
            return false;
        }
    }
    if !filter.card_classes.is_empty() {
        match &card.card_class {
            Some(class) if filter.card_classes.iter().any(|c| c == class) => {}
            _ => return false,
        }
    }
    if let Some(rarity) = &filter.rarity {
        if card.rarity.as_deref() != Some(rarity.as_str()) {
            return false;
        }
    }
    if !filter.card_sets.is_empty() {
        match &card.card_set {
            Some(set) if filter.card_sets.iter().any(|s| s == set) => {}
            _ => return false,
        }
    }
    if let Some(min) = filter.cost_min {
        if card.cost.is_none_or(|cost| cost < min) {
            return false;
        }
    }
    if let Some(max) = filter.cost_max {
        if card.cost.is_none_or(|cost| cost > max) {
            return false;
        }
    }
    true
}

/// 池求值入口(§12.1 核心模型,冻结签名)。
///
/// 流程:初始候选(kind 决定来源)→ 逐 filter 递减并记 Trace →
/// include 并入 / exclude 移除 → 权重 → 候选按 card_id 排序 → context_hash。
/// 动态池(FromDeck/FromGraveyard/From*History 等)要求 `state` 非 None,
/// 否则返回 [`PoolError::StateRequired`]。
pub fn resolve(
    spec: &PoolSpec,
    ctx: &PoolContext,
    cards: &dyn CardQuery,
    state: Option<&ObservedGameState>,
) -> Result<PoolSnapshot, PoolError> {
    spec.validate()?;
    if requires_state(spec) && state.is_none() {
        return Err(PoolError::StateRequired {
            pool_id: spec.pool_id.clone(),
        });
    }

    let catalog = load_catalog(ctx, cards)?;
    let (mut metas, exact) = initial_candidates(spec, ctx, cards, state, &catalog)?;
    dedup_metas(&mut metas);

    let mut trace = Vec::new();
    push_trace(&mut trace, "initial", 0, metas.len());

    for filter in &spec.filters {
        let before = metas.len();
        apply_filter(&mut metas, filter, ctx);
        push_trace(
            &mut trace,
            &filter_step_name(filter),
            before.saturating_sub(metas.len()),
            metas.len(),
        );
    }

    if !spec.include.is_empty() {
        apply_include(&mut metas, &spec.include, &catalog);
        push_trace(&mut trace, "include", 0, metas.len());
    }

    if !spec.exclude.is_empty() {
        let before = metas.len();
        let exclude: BTreeSet<&CardId> = spec.exclude.iter().collect();
        metas.retain(|meta| !exclude.contains(&meta.card_id));
        push_trace(
            &mut trace,
            "exclude",
            before.saturating_sub(metas.len()),
            metas.len(),
        );
    }

    let mut candidates: Vec<WeightedCard> = metas
        .into_iter()
        .map(|meta| WeightedCard {
            card_id: meta.card_id,
            dbf_id: meta.dbf_id,
            weight: 1.0,
        })
        .collect();
    apply_weights(&mut candidates, &spec.weight_rule);
    candidates.sort_by(|a, b| a.card_id.cmp(&b.card_id));

    Ok(PoolSnapshot {
        pool_id: spec.pool_id.clone(),
        build_id: ctx.build_id,
        context_hash: context_hash(ctx),
        candidates,
        trace,
        exact,
    })
}

fn kind_card_filter(kind: PoolKind) -> CardFilter {
    let mut filter = CardFilter::default();
    match kind {
        PoolKind::RandomMinion | PoolKind::RandomSummon => {
            filter.card_type = Some("MINION".into());
        }
        PoolKind::RandomSpell => {
            filter.card_type = Some("SPELL".into());
        }
        PoolKind::Discover
        | PoolKind::RandomCard
        | PoolKind::DynamicQuery
        | PoolKind::Explicit
        | PoolKind::Entourage
        | PoolKind::FromDeck
        | PoolKind::FromHand
        | PoolKind::FromGraveyard
        | PoolKind::FromPlayedHistory
        | PoolKind::FromGeneratedHistory => {}
    }
    filter
}

fn requires_state(spec: &PoolSpec) -> bool {
    matches!(
        spec.kind,
        PoolKind::FromDeck
            | PoolKind::FromHand
            | PoolKind::FromGraveyard
            | PoolKind::FromPlayedHistory
            | PoolKind::FromGeneratedHistory
    ) || spec.context_inputs.iter().any(|key| {
        matches!(
            key,
            ContextKey::Deck
                | ContextKey::Hand
                | ContextKey::Graveyard
                | ContextKey::PlayedHistory
                | ContextKey::GeneratedHistory
                | ContextKey::BoardEntities
        )
    })
}

fn load_catalog(
    ctx: &PoolContext,
    cards: &dyn CardQuery,
) -> Result<BTreeMap<CardId, CardMeta>, PoolError> {
    let rows = cards.query(ctx.build_id, &CardFilter::default())?;
    Ok(rows
        .into_iter()
        .map(|meta| (meta.card_id.clone(), meta))
        .collect())
}

fn initial_candidates(
    spec: &PoolSpec,
    ctx: &PoolContext,
    cards: &dyn CardQuery,
    state: Option<&ObservedGameState>,
    catalog: &BTreeMap<CardId, CardMeta>,
) -> Result<(Vec<CardMeta>, bool), PoolError> {
    match spec.kind {
        PoolKind::Explicit | PoolKind::Entourage => {
            Ok((metas_from_ids(&spec.include, catalog), true))
        }
        PoolKind::Discover
        | PoolKind::RandomCard
        | PoolKind::RandomMinion
        | PoolKind::RandomSpell
        | PoolKind::RandomSummon
        | PoolKind::DynamicQuery => {
            let filter = kind_card_filter(spec.kind);
            Ok((cards.query(ctx.build_id, &filter)?, true))
        }
        PoolKind::FromDeck => from_zone(state, catalog, Zone::Deck, true),
        PoolKind::FromHand => from_zone(state, catalog, Zone::Hand, true),
        PoolKind::FromGraveyard => from_zone(state, catalog, Zone::Graveyard, false),
        PoolKind::FromPlayedHistory => from_zones(state, catalog, &[Zone::Play], false),
        PoolKind::FromGeneratedHistory => from_generated(state, catalog),
    }
}

fn from_zone(
    state: Option<&ObservedGameState>,
    catalog: &BTreeMap<CardId, CardMeta>,
    zone: Zone,
    private_zone: bool,
) -> Result<(Vec<CardMeta>, bool), PoolError> {
    from_zones(state, catalog, &[zone], private_zone)
}

fn from_zones(
    state: Option<&ObservedGameState>,
    catalog: &BTreeMap<CardId, CardMeta>,
    zones: &[Zone],
    private_zone: bool,
) -> Result<(Vec<CardMeta>, bool), PoolError> {
    let state = state.expect("requires_state 已拦截");
    let friendly = state.friendly_player;
    let mut exact = true;
    let mut ids = Vec::new();
    for entity in state.entities.values() {
        if !zones.contains(&entity.zone) {
            continue;
        }
        if private_zone {
            if let Some(owner) = friendly {
                if entity.controller.is_some_and(|c| c != owner) {
                    continue;
                }
                if entity.controller.is_none() {
                    exact = false;
                    continue;
                }
            }
        }
        match &entity.card_id {
            Some(card_id) => ids.push(card_id.clone()),
            None => exact = false,
        }
    }
    if private_zone && !all_private_faces_known(state, zones, friendly) {
        exact = false;
    }
    Ok((metas_from_ids(&ids, catalog), exact))
}

fn all_private_faces_known(
    state: &ObservedGameState,
    zones: &[Zone],
    friendly: Option<PlayerId>,
) -> bool {
    state.entities.values().all(|entity| {
        if !zones.contains(&entity.zone) {
            return true;
        }
        if let Some(owner) = friendly {
            if entity.controller != Some(owner) {
                return true;
            }
        }
        entity.card_id.is_some()
    })
}

fn from_generated(
    state: Option<&ObservedGameState>,
    catalog: &BTreeMap<CardId, CardMeta>,
) -> Result<(Vec<CardMeta>, bool), PoolError> {
    let state = state.expect("requires_state 已拦截");
    let mut exact = true;
    let mut ids = Vec::new();
    for entity in state.entities.values() {
        if entity.source_entity.is_none() {
            continue;
        }
        match &entity.card_id {
            Some(card_id) => ids.push(card_id.clone()),
            None => exact = false,
        }
    }
    Ok((metas_from_ids(&ids, catalog), exact))
}

fn metas_from_ids(ids: &[CardId], catalog: &BTreeMap<CardId, CardMeta>) -> Vec<CardMeta> {
    ids.iter()
        .map(|id| catalog.get(id).cloned().unwrap_or_else(|| stub_meta(id)))
        .collect()
}

fn stub_meta(card_id: &CardId) -> CardMeta {
    CardMeta {
        card_id: card_id.clone(),
        dbf_id: None,
        card_type: None,
        card_set: None,
        rarity: None,
        card_class: None,
        cost: None,
        collectible: false,
    }
}

fn dedup_metas(metas: &mut Vec<CardMeta>) {
    let mut seen = BTreeSet::new();
    metas.retain(|meta| seen.insert(meta.card_id.clone()));
}

fn apply_filter(metas: &mut Vec<CardMeta>, filter: &PoolFilter, ctx: &PoolContext) {
    metas.retain(|meta| meta_matches_pool_filter(meta, filter, ctx));
}

fn meta_matches_pool_filter(meta: &CardMeta, filter: &PoolFilter, ctx: &PoolContext) -> bool {
    match filter {
        PoolFilter::FormatLegal => match legal_card_sets(ctx.format) {
            Some(sets) => meta
                .card_set
                .as_deref()
                .is_some_and(|set| sets.contains(&set)),
            None => true,
        },
        PoolFilter::CardType(card_type) => meta.card_type.as_deref() == Some(card_type.as_str()),
        PoolFilter::ClassMatches { include_neutral } => match &meta.card_class {
            Some(class) if ctx.player_class.iter().any(|c| c == class) => true,
            Some(class) if *include_neutral && class == "NEUTRAL" => true,
            _ => false,
        },
        PoolFilter::CardClass(class) => meta.card_class.as_deref() == Some(class.as_str()),
        PoolFilter::Rarity(rarity) => meta.rarity.as_deref() == Some(rarity.as_str()),
        PoolFilter::Collectible(flag) => meta.collectible == *flag,
        PoolFilter::Tribe(_) => {
            // CardMeta 冻结投影没有种族字段,无法判定;本级不裁剪。
            true
        }
        PoolFilter::CostBetween { min, max } => {
            let Some(cost) = meta.cost else {
                return false;
            };
            min.is_none_or(|bound| cost >= bound) && max.is_none_or(|bound| cost <= bound)
        }
        PoolFilter::ExcludeSource => meta.card_id != ctx.source_card,
    }
}

fn filter_step_name(filter: &PoolFilter) -> String {
    match filter {
        PoolFilter::FormatLegal => "format_legal".into(),
        PoolFilter::CardType(card_type) => format!("card_type={card_type}"),
        PoolFilter::ClassMatches { .. } => "class_matches".into(),
        PoolFilter::CardClass(class) => format!("card_class={class}"),
        PoolFilter::Rarity(rarity) => format!("rarity={rarity}"),
        PoolFilter::Collectible(flag) => format!("collectible={flag}"),
        PoolFilter::Tribe(tribe) => format!("tribe={tribe}"),
        PoolFilter::CostBetween { min, max } => match (min, max) {
            (Some(lo), Some(hi)) => format!("cost_between={lo}..={hi}"),
            (Some(lo), None) => format!("cost_between={lo}.."),
            (None, Some(hi)) => format!("cost_between..={hi}"),
            (None, None) => "cost_between".into(),
        },
        PoolFilter::ExcludeSource => "exclude_source".into(),
    }
}

fn apply_include(
    metas: &mut Vec<CardMeta>,
    include: &[CardId],
    catalog: &BTreeMap<CardId, CardMeta>,
) {
    let existing: BTreeSet<CardId> = metas.iter().map(|meta| meta.card_id.clone()).collect();
    for id in include {
        if !existing.contains(id) {
            metas.push(catalog.get(id).cloned().unwrap_or_else(|| stub_meta(id)));
        }
    }
}

fn apply_weights(candidates: &mut [WeightedCard], rule: &crate::WeightRule) {
    match rule {
        crate::WeightRule::Uniform => {
            for card in candidates.iter_mut() {
                card.weight = 1.0;
            }
        }
        crate::WeightRule::Explicit(table) => {
            let map: BTreeMap<&CardId, f64> = table.iter().map(|(id, w)| (id, *w)).collect();
            for card in candidates.iter_mut() {
                card.weight = map.get(&card.card_id).copied().unwrap_or(1.0);
            }
        }
    }
}

fn push_trace(trace: &mut Vec<PoolTraceEntry>, step: &str, removed: usize, remaining: usize) {
    trace.push(PoolTraceEntry {
        step: step.to_string(),
        removed,
        remaining,
    });
}
