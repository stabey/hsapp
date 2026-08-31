//! 直接效果执行器(design §4 子集;未支持变体显式 Unsupported)。
//!
//! 边界:
//! - 执行 `EffectProgram.play`,并派发**自身**的打出期触发
//!   ([`DISPATCHED_EVENTS`]:Battlecry / OnPlay / AfterPlay);其余
//!   `TriggerEvent` 变体记 trace `not dispatched`,不静默丢弃;
//! - 有效序列最多一个顶层随机操作(Discover / RandomChoice);
//!   多个随机或随机嵌在 Repeat/ForEach/Conditional 内 →
//!   `Unsupported(NotImplemented { what: "multiple random ops" })`
//!   或 `"nested random op"`;
//! - 每个确定性操作后统一结算死亡；单亡语可级联，未知/复杂顺序保守退出。

use crate::apply::{
    CardLookup, PoolResolver, RuleLookup, SimCardDefinition, SimContext, SimOutcome, SimRequest,
    TargetBinding,
};
use crate::outcome::{
    ChanceKind, OutcomeSet, ReproMeta, RuleTrace, RuleUse, SampleMethod, SampledOutcomeSet,
    TraceEntry, UnsupportedReason, WeightedOutcome,
};
use crate::rng::{checked_comb, k_combinations, sample_combination, sample_weighted, SplitMix64};
use crate::select::{all_predicates, resolve_player_ref, resolve_selector, SelectError};
use crate::state::SimGameState;
use crate::SimError;
use hs_effect_ir::{CardRef, EffectProgram, Operation, PlayerRef, Selector, TriggerEvent};
use hs_pools::{PoolSnapshot, WeightedCard};
use hs_types::{BuildId, CardId, CardType, EntityId, PlayerId, RuleStatus, Zone};
use std::collections::{BTreeMap, BTreeSet};

/// 打出一张牌时派发的自身触发事件(按 `program.triggers` 的声明顺序)。
///
/// 其余已冻结的 `TriggerEvent` 变体不在本执行器的派发范围内:遇到时记
/// trace `not dispatched`,由调用方(M4 搜索)或后续步骤的死亡结算负责。
/// 死亡→亡语→再死亡的最大层数。超限整体转 `Unsupported(RecursionLimit)`。
///
/// 取 8:真实对局里亡语连锁超过 3 层已极罕见,8 给足余量又能防病态循环。
const MAX_DEATH_DEPTH: u32 = 8;
const MAX_BOARD_SLOTS: usize = 7;

const DISPATCHED_EVENTS: [TriggerEvent; 3] = [
    TriggerEvent::Battlecry,
    TriggerEvent::OnPlay,
    TriggerEvent::AfterPlay,
];

/// 光环类触发事件。本执行器**不实装光环**(无 buff 分层),场上一旦有带这些
/// 触发的卡就显式不支持 —— 静默按无光环算会让 M4 的斩杀判断出错。
const AURA_EVENTS: [TriggerEvent; 3] = [
    TriggerEvent::AuraEnter,
    TriggerEvent::AuraLeave,
    TriggerEvent::AuraRecalculate,
];

pub(crate) fn validate_strict_state(state: &SimGameState) -> Result<(), UnsupportedReason> {
    if let Some(entity) = state.entities.values().find(|entity| {
        entity.zone == Zone::Play
            && matches!(
                entity.card_type,
                None | Some(CardType::Invalid | CardType::Unknown(_))
            )
    }) {
        return Err(UnsupportedReason::InsufficientState {
            what: format!(
                "strict simulation requires known card_type for in-play entity {}",
                entity.entity_id.0
            ),
        });
    }
    Ok(())
}

pub(crate) fn apply_action_program(
    state: &SimGameState,
    source: EntityId,
    target: Option<EntityId>,
    context: &SimContext<'_>,
    req: &SimRequest,
    action_name: &str,
) -> Result<SimOutcome, SimError> {
    let Some(card_id) = state
        .entity(source)
        .and_then(|entity| entity.card_id.as_ref())
    else {
        return Ok(action_unsupported(
            action_name,
            UnsupportedReason::InsufficientState {
                what: format!("action source {} missing card id", source.0),
            },
            req,
        ));
    };
    let Some(program) = context.rules.program_for(card_id) else {
        let mut outcome = action_unsupported(
            action_name,
            UnsupportedReason::InsufficientState {
                what: format!("rule missing: {card_id}"),
            },
            req,
        );
        outcome.unknown_rule_cards.push(card_id.clone());
        return Ok(outcome);
    };

    let mut outcome = match run(
        state,
        program,
        &TargetBinding {
            source: Some(source),
            target,
        },
        context.pools,
        context.rules,
        Some((context.cards, context.build)),
        req,
    ) {
        Ok(outcome) => outcome,
        Err(SimError::InvalidBinding(message)) => {
            return Ok(action_unsupported(
                action_name,
                UnsupportedReason::InsufficientState {
                    what: format!("action target binding incomplete: {message}"),
                },
                req,
            ));
        }
        Err(error) => return Err(error),
    };
    outcome.trace.entries.insert(
        0,
        TraceEntry {
            op: "action".into(),
            detail: action_name.into(),
        },
    );
    Ok(outcome)
}

pub(crate) fn apply_combat_action(
    state: &SimGameState,
    friendly: PlayerId,
    attacker: EntityId,
    defender: EntityId,
    context: &SimContext<'_>,
    req: &SimRequest,
) -> Result<SimOutcome, SimError> {
    let aura_scan = scan_auras(state, context.rules, Some(context.build));
    if !aura_scan.is_empty() {
        return Ok(unsupported(
            OutcomeSet::Unsupported(aura_scan.unsupported_reason(Some(context.build))),
            RuleTrace {
                entries: vec![aura_scan.trace_entry()],
            },
            action_meta("attack", req),
            aura_scan.evidence(),
        ));
    }
    let mut next = state.clone();
    let weapon_to_break = match validate_and_consume_attack(&mut next, friendly, attacker, defender)
    {
        Ok(weapon) => weapon,
        Err(reason) => return Ok(action_unsupported("attack", reason, req)),
    };

    let mut ctx = ApplyCtx {
        state: next,
        binding: TargetBinding {
            source: Some(attacker),
            target: Some(defender),
        },
        pools: context.pools,
        rules: context.rules,
        trace: RuleTrace {
            entries: vec![TraceEntry {
                op: "action".into(),
                detail: format!("attack attacker={} defender={}", attacker.0, defender.0),
            }],
        },
        evidence: Evidence::default(),
        card_lookup: Some(context.cards),
        build: Some(context.build),
        strict_indirect_rules: true,
        pending_deaths: weapon_to_break.into_iter().collect(),
        in_deathrattle: false,
    };
    if let Err(fail) = ctx.apply_combat_damage(attacker, defender) {
        return match fail {
            ApplyFail::Unsupported(reason) => Ok(action_unsupported("attack", reason, req)),
            ApplyFail::Error(error) => Err(error),
        };
    }
    if let Err(fail) = ctx.resolve_deaths() {
        return match fail {
            ApplyFail::Unsupported(reason) => Ok(unsupported(
                OutcomeSet::Unsupported(reason),
                ctx.trace,
                action_meta("attack", req),
                ctx.evidence,
            )),
            ApplyFail::Error(error) => Err(error),
        };
    }
    Ok(finish_outcome(
        OutcomeSet::Deterministic(ctx.state),
        ctx.trace,
        action_meta("attack", req),
        ctx.evidence,
    ))
}

/// 扫描场上全部光环规则,同时保留 Build 不匹配的覆盖缺口。
///
/// 查不到规则的卡**不算光环** —— 未知不等于有,覆盖率问题由
/// `unknown_rule_cards` 与 M4 的置信度处理。空规则表下必然扫不出结果,
/// 故 `apply_program` 只在 trace 记 `unchecked`,不谎称「场上没有光环」。
fn scan_auras<'a>(
    state: &SimGameState,
    rules: &'a dyn RuleLookup,
    build: Option<BuildId>,
) -> AuraScan<'a> {
    let mut scan = AuraScan::default();
    for card in state
        .entities
        .values()
        .filter(|entity| entity.zone == Zone::Play)
        .filter_map(|entity| entity.card_id.clone())
    {
        let Some(program) = rules.program_for(&card) else {
            continue;
        };
        if !program
            .triggers
            .iter()
            .any(|trigger| AURA_EVENTS.contains(&trigger.event))
        {
            continue;
        }
        if build.is_none_or(|current| program.build_range.contains(current)) {
            scan.current.insert(card, program);
        } else {
            scan.wrong_build.insert(card);
        }
    }
    scan
}

#[derive(Default)]
struct AuraScan<'a> {
    current: BTreeMap<CardId, &'a EffectProgram>,
    wrong_build: BTreeSet<CardId>,
}

impl AuraScan<'_> {
    fn is_empty(&self) -> bool {
        self.current.is_empty() && self.wrong_build.is_empty()
    }

    fn evidence(&self) -> Evidence {
        let mut evidence = Evidence::default();
        for program in self.current.values() {
            evidence.register_rule(program);
        }
        evidence
            .unknown_rule_cards
            .extend(self.wrong_build.iter().cloned());
        evidence
    }

    fn trace_entry(&self) -> TraceEntry {
        let cards = self
            .current
            .keys()
            .chain(self.wrong_build.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|card| card.to_string())
            .collect::<Vec<_>>()
            .join(",");
        TraceEntry {
            op: "aura".into(),
            detail: format!("unsupported(cards={cards})"),
        }
    }

    fn unsupported_reason(&self, build: Option<BuildId>) -> UnsupportedReason {
        if self.wrong_build.is_empty() {
            UnsupportedReason::NotImplemented {
                what: "aura".into(),
            }
        } else {
            let cards = self
                .wrong_build
                .iter()
                .map(CardId::as_str)
                .collect::<Vec<_>>()
                .join(",");
            UnsupportedReason::InsufficientState {
                what: format!("aura rule build mismatch: cards={cards} build={build:?}"),
            }
        }
    }
}

/// 构造有效操作序列:`play` 后接各已派发自身触发的 `effects`(声明序)。
///
/// `when` 谓词对**入参局面**求值 —— 打出期触发(Battlecry/OnPlay/AfterPlay)
/// 的条件本就描述打出那一刻的场面。谓词判不出来(状态不足)时**不猜**:
/// 整个执行转 `Unsupported(InsufficientState)`,因为「该不该触发」未知
/// 意味着结果局面未知。
fn effective_ops(
    state: &SimGameState,
    binding: &TargetBinding,
    program: &EffectProgram,
    trace: &mut RuleTrace,
) -> Result<Vec<Operation>, ApplyFail> {
    let mut ops = program.play.clone();
    for trigger in &program.triggers {
        if !DISPATCHED_EVENTS.contains(&trigger.event) {
            trace.entries.push(TraceEntry {
                op: "trigger".into(),
                detail: format!("{:?} not dispatched", trigger.event),
            });
            continue;
        }
        match all_predicates(state, binding, &trigger.when) {
            Ok(true) => {
                trace.entries.push(TraceEntry {
                    op: "trigger".into(),
                    detail: format!(
                        "{:?} dispatched ({} ops)",
                        trigger.event,
                        trigger.effects.len()
                    ),
                });
                ops.extend(trigger.effects.iter().cloned());
            }
            Ok(false) => trace.entries.push(TraceEntry {
                op: "trigger".into(),
                detail: format!("{:?} skipped (predicate)", trigger.event),
            }),
            Err(SelectError::Binding(msg)) => {
                return Err(ApplyFail::Error(SimError::InvalidBinding(msg)))
            }
            Err(SelectError::Insufficient(what)) => {
                return Err(ApplyFail::Unsupported(
                    UnsupportedReason::InsufficientState {
                        what: format!("trigger {:?} 的 when 谓词判不了: {what}", trigger.event),
                    },
                ))
            }
        }
    }
    Ok(ops)
}

pub(crate) fn run(
    state: &SimGameState,
    program: &EffectProgram,
    binding: &TargetBinding,
    pools: &dyn PoolResolver,
    rules: &dyn RuleLookup,
    card_source: Option<(&dyn CardLookup, BuildId)>,
    req: &SimRequest,
) -> Result<SimOutcome, SimError> {
    let mut meta = ReproMeta {
        seed: req.seed,
        branch_budget: req.branch_budget,
        method: SampleMethod::Exact,
        truncation: None,
        chance_kind: None,
        rule_version: format_rule_version(program),
        pool_snapshot_hashes: Vec::new(),
    };

    let mut evidence = Evidence::default();
    evidence.register_rule(program);

    if let Some((_, build)) = card_source {
        if state.build_id != Some(build) {
            return Ok(unsupported(
                OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                    what: format!(
                        "strict simulation build mismatch: state={:?} context={}",
                        state.build_id, build.0
                    ),
                }),
                RuleTrace::default(),
                meta,
                evidence,
            ));
        }
        if !program.build_range.contains(build) {
            return Ok(unsupported(
                OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                    what: format!(
                        "root rule build mismatch: card={} build={}",
                        program.card_id, build.0
                    ),
                }),
                RuleTrace::default(),
                meta,
                evidence,
            ));
        }
        if let Err(reason) = validate_strict_state(state) {
            return Ok(unsupported(
                OutcomeSet::Unsupported(reason),
                RuleTrace::default(),
                meta,
                evidence,
            ));
        }
    }

    // Unknown 表示没有可执行语义，不能与真正的 Verified 白板混成同一个
    // Deterministic no-op。即使旧入口也必须守住根规则边界。
    if program.status == RuleStatus::Unknown {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                what: format!("root rule status unknown: {}", program.card_id),
            }),
            RuleTrace::default(),
            meta,
            evidence,
        ));
    }

    if let Some(handler) = &program.custom_handler {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::CustomHandler {
                handler: handler.to_string(),
            }),
            RuleTrace::default(),
            meta,
            evidence,
        ));
    }

    // 有效序列 = play ++ 各已派发自身触发的 effects(声明序)。
    let mut trace = RuleTrace::default();

    // 光环未实装:场上有光环卡就显式不支持,不静默按无光环算(R6)。
    // 没发现光环时**不记 trace** —— 无信息量的条目会淹没真正的执行轨迹。
    // 注意 `apply_program`(空规则表)查不到任何规则,因而永远发现不了光环;
    // 要让这道检查生效必须走 `apply_program_with_rules`(M4 搜索侧即如此)。
    let aura_build = card_source.map(|(_, build)| build).or(state.build_id);
    let aura_scan = scan_auras(state, rules, aura_build);
    if !aura_scan.is_empty() {
        evidence.merge(aura_scan.evidence());
        trace.entries.push(aura_scan.trace_entry());
        return Ok(unsupported(
            OutcomeSet::Unsupported(aura_scan.unsupported_reason(aura_build)),
            trace,
            meta,
            evidence,
        ));
    }

    let ops = match effective_ops(state, binding, program, &mut trace) {
        Ok(ops) => ops,
        Err(ApplyFail::Unsupported(reason)) => {
            return Ok(unsupported(
                OutcomeSet::Unsupported(reason),
                trace,
                meta,
                evidence,
            ));
        }
        Err(ApplyFail::Error(err)) => return Err(err),
    };

    if count_random_ops(&ops) > 1 {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::NotImplemented {
                what: "multiple random ops".into(),
            }),
            trace,
            meta,
            evidence,
        ));
    }
    if has_nested_random(&ops) {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::NotImplemented {
                what: "nested random op".into(),
            }),
            trace,
            meta,
            evidence,
        ));
    }

    let random_at = ops.iter().position(is_random_op);
    let mut ctx = ApplyCtx {
        state: state.clone(),
        binding: *binding,
        pools,
        rules,
        trace,
        evidence,
        card_lookup: card_source.map(|(cards, _)| cards),
        build: card_source.map(|(_, build)| build),
        strict_indirect_rules: card_source.is_some(),
        pending_deaths: std::collections::BTreeSet::new(),
        in_deathrattle: false,
    };

    let prefix_end = random_at.unwrap_or(ops.len());
    match ctx.run_ops(&ops[..prefix_end]) {
        Ok(()) => {}
        Err(ApplyFail::Unsupported(reason)) => {
            return Ok(unsupported(
                OutcomeSet::Unsupported(reason),
                ctx.trace,
                meta,
                ctx.evidence,
            ));
        }
        Err(ApplyFail::Error(err)) => return Err(err),
    }

    let Some(idx) = random_at else {
        return Ok(finish_outcome(
            OutcomeSet::Deterministic(ctx.state),
            ctx.trace,
            meta,
            ctx.evidence,
        ));
    };

    let (pool_id, options, count, chance_kind) = match &ops[idx] {
        Operation::Discover {
            pool,
            options,
            count,
        } => (
            pool.clone(),
            Some(*options),
            *count,
            ChanceKind::PlayerChoice,
        ),
        Operation::RandomChoice { pool, count } => (pool.clone(), None, *count, ChanceKind::Random),
        other => {
            return Ok(unsupported(
                OutcomeSet::Unsupported(UnsupportedReason::NotImplemented {
                    what: format!("internal: expected random op, got {other:?}"),
                }),
                ctx.trace,
                meta,
                ctx.evidence,
            ));
        }
    };
    meta.chance_kind = Some(chance_kind);

    let snapshot = ctx.pools.snapshot(&pool_id)?;
    ctx.evidence
        .pool_hashes
        .insert(snapshot.context_hash.clone());
    if ctx.build.is_some_and(|build| snapshot.build_id != build) {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                what: format!(
                    "pool snapshot build mismatch: pool={} snapshot={} context={}",
                    pool_id,
                    snapshot.build_id.0,
                    ctx.build.expect("checked strict build").0
                ),
            }),
            ctx.trace,
            meta,
            ctx.evidence,
        ));
    }
    ctx.trace.entries.push(TraceEntry {
        op: if options.is_some() {
            "discover".into()
        } else {
            "random_choice".into()
        },
        detail: format!(
            "pool={} candidates={} options={:?} count={count}",
            pool_id,
            snapshot.candidates.len(),
            options
        ),
    });

    if snapshot.candidates.is_empty() {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                what: "empty pool".into(),
            }),
            ctx.trace,
            meta,
            ctx.evidence,
        ));
    }

    // Sampled 也必须覆盖整个候选池，不能因为固定 seed 恰好没抽中缺定义的
    // 卡就返回一个“幸运成功”结果。
    if let Err(fail) = ctx.ensure_card_definitions(
        snapshot
            .candidates
            .iter()
            .map(|candidate| &candidate.card_id),
    ) {
        match fail {
            ApplyFail::Unsupported(reason) => {
                return Ok(unsupported(
                    OutcomeSet::Unsupported(reason),
                    ctx.trace,
                    meta,
                    ctx.evidence,
                ));
            }
            ApplyFail::Error(err) => return Err(err),
        }
    }

    let n = snapshot.candidates.len();
    let k = count as usize;
    if let Some(shown) = options {
        if (shown as usize) < k {
            return Ok(unsupported(
                OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                    what: "discover count exceeds options".into(),
                }),
                ctx.trace,
                meta,
                ctx.evidence,
            ));
        }
    }
    if k > n {
        return Ok(unsupported(
            OutcomeSet::Unsupported(UnsupportedReason::InsufficientState {
                what: "choice count exceeds pool size".into(),
            }),
            ctx.trace,
            meta,
            ctx.evidence,
        ));
    }

    let total_branches = checked_comb(n as u64, k as u64);
    let over_budget = match total_branches {
        Some(total) => total > u64::from(req.branch_budget),
        None => true,
    };

    let suffix = &ops[idx + 1..];
    let self_id = match resolve_player_ref(&ctx.state, &ctx.binding, PlayerRef::SelfPlayer) {
        Ok(ids) => ids.into_iter().next(),
        Err(SelectError::Binding(msg)) => return Err(SimError::InvalidBinding(msg)),
        Err(SelectError::Insufficient(what)) => {
            return Ok(unsupported(
                OutcomeSet::Unsupported(UnsupportedReason::InsufficientState { what }),
                ctx.trace,
                meta,
                ctx.evidence,
            ));
        }
    };

    if over_budget {
        meta.method = SampleMethod::MonteCarlo;
        meta.truncation = Some(match total_branches {
            Some(total) => format!("branch_budget exceeded: {total} > {}", req.branch_budget),
            None => "branch count overflow".into(),
        });
        let samples = sample_branches(&ctx, &snapshot, k, req, suffix, self_id)?;
        match samples {
            Ok(sampled) => {
                ctx.evidence.merge(sampled.evidence);
                return Ok(finish_outcome(
                    OutcomeSet::Sampled(SampledOutcomeSet {
                        samples: sampled.samples,
                        method: SampleMethod::MonteCarlo,
                        total_branches,
                    }),
                    ctx.trace,
                    meta,
                    ctx.evidence,
                ));
            }
            Err(branch_fail) => {
                ctx.evidence.merge(branch_fail.evidence);
                return Ok(unsupported(
                    OutcomeSet::Unsupported(branch_fail.reason),
                    ctx.trace,
                    meta,
                    ctx.evidence,
                ));
            }
        }
    }

    let combinations = k_combinations(n, k);
    let weights = combination_weights(&snapshot.candidates, &combinations);
    let weight_sum: f64 = weights.iter().sum();
    let mut branches = Vec::with_capacity(combinations.len());
    let mut first_failure = None;
    for (combo, weight) in combinations.into_iter().zip(weights) {
        let cards: Vec<WeightedCard> = combo
            .iter()
            .map(|&i| snapshot.candidates[i].clone())
            .collect();
        match finish_branch(&ctx, &cards, suffix, self_id)? {
            Ok(branch) => {
                let probability = if weight_sum > 0.0 {
                    weight / weight_sum
                } else {
                    0.0
                };
                branches.push(WeightedOutcome {
                    probability,
                    state: branch.state,
                    note: Some(note_for(&cards)),
                });
                ctx.evidence.merge(branch.evidence);
            }
            Err(branch_fail) => {
                ctx.evidence.merge(branch_fail.evidence);
                if first_failure.is_none() {
                    first_failure = Some(branch_fail.reason);
                }
            }
        }
    }

    if let Some(reason) = first_failure {
        return Ok(unsupported(
            OutcomeSet::Unsupported(reason),
            ctx.trace,
            meta,
            ctx.evidence,
        ));
    }

    Ok(finish_outcome(
        OutcomeSet::Exact(branches),
        ctx.trace,
        meta,
        ctx.evidence,
    ))
}

#[derive(Clone, Default)]
struct Evidence {
    pool_hashes: BTreeSet<String>,
    unknown_rule_cards: BTreeSet<CardId>,
    rule_uses: BTreeMap<(CardId, String), RuleUse>,
}

impl Evidence {
    fn register_rule(&mut self, program: &EffectProgram) {
        let rule_version = format_rule_version(program);
        let rule_use = RuleUse {
            card_id: program.card_id.clone(),
            status: program.status,
            confidence: program.confidence,
            rule_version: rule_version.clone(),
        };
        self.rule_uses
            .entry((program.card_id.clone(), rule_version))
            .or_insert(rule_use);
    }

    fn merge(&mut self, other: Evidence) {
        self.pool_hashes.extend(other.pool_hashes);
        self.unknown_rule_cards.extend(other.unknown_rule_cards);
        for (key, rule_use) in other.rule_uses {
            self.rule_uses.entry(key).or_insert(rule_use);
        }
    }
}

struct ApplyCtx<'a> {
    state: SimGameState,
    binding: TargetBinding,
    pools: &'a dyn PoolResolver,
    rules: &'a dyn RuleLookup,
    trace: RuleTrace,
    evidence: Evidence,
    card_lookup: Option<&'a dyn CardLookup>,
    build: Option<BuildId>,
    strict_indirect_rules: bool,
    /// `Destroy` 标记的待死实体(生命未必归零,故不能靠血量收集)。
    pending_deaths: std::collections::BTreeSet<EntityId>,
    /// 当前是否在结算亡语体(只影响未支持原因的措辞,便于定位)。
    in_deathrattle: bool,
}

enum ApplyFail {
    Unsupported(UnsupportedReason),
    Error(SimError),
}

#[derive(Clone, Copy)]
enum DamageKind {
    Effect,
    Fatigue,
}

impl DamageKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Effect => "damage",
            Self::Fatigue => "fatigue",
        }
    }

    const fn is_blocked_by_divine_shield(self) -> bool {
        matches!(self, Self::Effect)
    }
}

impl ApplyCtx<'_> {
    fn card_definition(&self, card_id: &CardId) -> Result<Option<SimCardDefinition>, ApplyFail> {
        let (Some(lookup), Some(build)) = (self.card_lookup, self.build) else {
            return Ok(None);
        };
        let Some(definition) = lookup.sim_card(build, card_id) else {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!("card definition missing: card={card_id} build={}", build.0),
                },
            ));
        };
        if definition.card_id != *card_id {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!(
                        "card definition identity mismatch: requested={card_id} returned={}",
                        definition.card_id
                    ),
                },
            ));
        }
        if matches!(definition.card_type, CardType::Unknown(_)) {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!(
                        "card definition type unknown: card={card_id} build={}",
                        build.0
                    ),
                },
            ));
        }
        Ok(Some(definition))
    }

    fn ensure_card_definitions<'b>(
        &self,
        cards: impl IntoIterator<Item = &'b CardId>,
    ) -> Result<(), ApplyFail> {
        if self.card_lookup.is_none() {
            return Ok(());
        }
        for card in cards {
            self.card_definition(card)?;
        }
        Ok(())
    }

    /// 顶层执行:每个 Operation 跑完后进入死亡结算(D1)。
    fn run_ops(&mut self, ops: &[Operation]) -> Result<(), ApplyFail> {
        for op in ops {
            self.run_one(op)?;
            self.resolve_deaths()?;
        }
        Ok(())
    }

    /// 亡语体执行:不在每个 op 后单独结算 —— 由外层结算循环的下一轮统一收集,
    /// 亡语的 effects 作为一个整体构成本层效果。
    fn run_ops_no_resolve(&mut self, ops: &[Operation]) -> Result<(), ApplyFail> {
        for op in ops {
            self.run_one(op)?;
        }
        Ok(())
    }

    /// 死亡结算阶段(design §3/§3.0/§3.1)。
    ///
    /// 循环:收集本层死亡 → 检查多亡语顺序 → 全部离场(D3)→ 结算亡语 →
    /// 下一层。深度超限整体转 `Unsupported(RecursionLimit)`,不返回半截状态。
    fn resolve_deaths(&mut self) -> Result<(), ApplyFail> {
        let mut depth = 0u32;
        loop {
            let dying = self.collect_dying();
            if dying.is_empty() {
                return Ok(());
            }
            if depth >= MAX_DEATH_DEPTH {
                return Err(ApplyFail::Unsupported(UnsupportedReason::RecursionLimit));
            }

            // 先把规则解析成 owned 快照，避免在亡语执行期间持有 rules 借用。
            // Unknown 与查不到规则同属 runtime unknown；Unknown 不得因恰好是
            // 空程序而被当成“已知没有亡语”。
            let mut with_dr: Vec<(EntityId, CardId, EffectProgram)> = Vec::new();
            let mut unknown_cards = BTreeSet::new();
            for id in &dying {
                let Some(card) = self.state.entity(*id).and_then(|e| e.card_id.clone()) else {
                    if self.strict_indirect_rules {
                        return Err(ApplyFail::Unsupported(
                            UnsupportedReason::InsufficientState {
                                what: format!("death entity {} missing card id", id.0),
                            },
                        ));
                    }
                    continue;
                };
                match self.rules.program_for(&card) {
                    Some(program) if program.status != RuleStatus::Unknown => {
                        if self.strict_indirect_rules {
                            let Some(build) = self.build else {
                                return Err(ApplyFail::Unsupported(
                                    UnsupportedReason::InsufficientState {
                                        what: "strict death resolution missing build".into(),
                                    },
                                ));
                            };
                            if !program.build_range.contains(build) {
                                self.evidence.unknown_rule_cards.insert(card.clone());
                                return Err(ApplyFail::Unsupported(
                                    UnsupportedReason::InsufficientState {
                                        what: format!(
                                            "deathrattle rule build mismatch: card={card} build={}",
                                            build.0
                                        ),
                                    },
                                ));
                            }
                        }
                        if program.triggers.iter().any(|trigger| {
                            trigger.event == TriggerEvent::Deathrattle
                                && !trigger.effects.is_empty()
                        }) {
                            with_dr.push((*id, card, program.clone()));
                        }
                    }
                    Some(_) | None => {
                        unknown_cards.insert(card);
                    }
                }
            }

            for card in &unknown_cards {
                self.evidence.unknown_rule_cards.insert(card.clone());
            }

            if self.strict_indirect_rules {
                if let Some(card) = unknown_cards.first() {
                    self.trace.entries.push(TraceEntry {
                        op: "deathrattle".into(),
                        detail: format!("unknown(card={card})"),
                    });
                    return Err(ApplyFail::Unsupported(
                        UnsupportedReason::InsufficientState {
                            what: format!("deathrattle rule missing or unknown: {card}"),
                        },
                    ));
                }
            }

            // §3.0:真实入场顺序不可知,故只在「多个已知亡语」时顺序才影响
            // 结果 —— 那时显式不支持,而不是猜一个 11/14 的近似序。
            if with_dr.len() >= 2 {
                return Err(ApplyFail::Unsupported(UnsupportedReason::NotImplemented {
                    what: "multi-deathrattle ordering".into(),
                }));
            }

            // D3:先全部离场,再结算亡语。真实日志 6/6 个样本均先入墓,
            // 再于 DEATHS 关闭后的兄弟块触发;见 death-ordering-evidence.md。
            for id in &dying {
                self.pending_deaths.remove(id);
                set_zone(&mut self.state, *id, Zone::Graveyard);
                self.trace.entries.push(TraceEntry {
                    op: "death".into(),
                    detail: format!("entity={} -> Graveyard depth={depth}", id.0),
                });
            }

            // 兼容入口保留既有 trace 时序：实体先离场，再报告未知亡语。
            for card in &unknown_cards {
                self.trace.entries.push(TraceEntry {
                    op: "deathrattle".into(),
                    detail: format!("unknown(card={card})"),
                });
            }

            // 至多一个亡语(上面已保证),结算它。
            for (id, card, program) in with_dr {
                let effects = program
                    .triggers
                    .iter()
                    .filter(|tr| tr.event == TriggerEvent::Deathrattle)
                    .flat_map(|tr| tr.effects.iter().cloned())
                    .collect::<Vec<_>>();
                self.evidence.register_rule(&program);
                self.trace.entries.push(TraceEntry {
                    op: "deathrattle".into(),
                    detail: format!("entity={} card={card} ops={}", id.0, effects.len()),
                });
                // 亡语的来源是死亡实体自身(SelfEntity/Source 绑定到它)。
                let saved = self.binding;
                self.binding.source = Some(id);
                self.in_deathrattle = true;
                let result = self.run_ops_no_resolve(&effects);
                self.in_deathrattle = false;
                self.binding = saved;
                result?;
            }

            depth += 1;
        }
    }

    /// 本层死亡集合:血量归零的在场实体 + `Destroy` 标记的待死实体。
    ///
    /// 按 `entity_id` 升序(BTreeMap 序),保可复现。**排除 `health == 0` 的实体**:
    /// M3 的 Summon/CreateCard 因无卡牌库给新实体填 0/0 攻血并在 trace 注明
    /// "stats unknown",血量未知不等于已死,否则召唤出来的随从会立刻暴毙。
    fn collect_dying(&self) -> Vec<EntityId> {
        let mut ids: std::collections::BTreeSet<EntityId> = self
            .state
            .entities
            .values()
            .filter(|e| {
                e.zone == Zone::Play
                    && e.dies_on_zero_health()
                    && e.health > 0
                    && e.current_health() <= 0
            })
            .map(|e| e.entity_id)
            .collect();
        for id in &self.pending_deaths {
            if self.state.entity(*id).is_some_and(|e| e.zone == Zone::Play) {
                ids.insert(*id);
            }
        }
        ids.into_iter().collect()
    }

    fn run_one(&mut self, op: &Operation) -> Result<(), ApplyFail> {
        match op {
            Operation::Draw { player, count } => self.op_draw(*player, *count),
            Operation::Damage { target, amount } => self.op_damage(target, *amount),
            Operation::Heal { target, amount } => self.op_heal(target, *amount),
            Operation::GainArmor { player, amount } => self.op_gain_armor(*player, *amount),
            Operation::Summon {
                player,
                card,
                count,
            } => self.op_summon(*player, card, *count, Zone::Play, "summon"),
            Operation::Destroy { target } => self.op_destroy(target),
            Operation::MoveZone { target, to } => self.op_move_zone(target, *to),
            Operation::CreateCard {
                player,
                card,
                zone,
                count,
            } => self.op_summon(*player, card, *count, *zone, "create_card"),
            Operation::ModifyTag { target, tag, value } => self.op_modify_tag(target, tag, *value),
            Operation::RefreshMana { player, amount } => self.op_refresh_mana(*player, *amount),
            Operation::Overload { player, amount } => self.op_overload(*player, *amount),
            Operation::Conditional {
                when,
                then,
                otherwise,
            } => {
                let holds = all_predicates(&self.state, &self.binding, when).map_err(map_select)?;
                self.trace.entries.push(TraceEntry {
                    op: "conditional".into(),
                    detail: format!("when={holds}"),
                });
                if holds {
                    self.run_ops(then)
                } else {
                    self.run_ops(otherwise)
                }
            }
            Operation::ForEach { over, effects } => {
                let ids = resolve_selector(&self.state, &self.binding, over).map_err(map_select)?;
                self.trace.entries.push(TraceEntry {
                    op: "for_each".into(),
                    detail: format!("count={}", ids.len()),
                });
                let saved = self.binding;
                for id in ids {
                    self.binding.target = Some(id);
                    self.run_ops(effects)?;
                }
                self.binding = saved;
                Ok(())
            }
            Operation::Repeat { times, effects } => {
                self.trace.entries.push(TraceEntry {
                    op: "repeat".into(),
                    detail: format!("times={times}"),
                });
                for _ in 0..*times {
                    self.run_ops(effects)?;
                }
                Ok(())
            }
            Operation::Discover { .. } | Operation::RandomChoice { .. } => {
                // 顶层随机由 run() 分叉;落到这里说明它嵌在控制流或亡语体里。
                // 分开措辞:亡语里的随机是本任务引入的新路径(如警卫犬 JAIL_878
                // 的随机召唤),报「nested random op」会误导定位。
                Err(ApplyFail::Unsupported(UnsupportedReason::NotImplemented {
                    what: if self.in_deathrattle {
                        "random op in deathrattle".into()
                    } else {
                        "nested random op".into()
                    },
                }))
            }
            Operation::Transform { .. } => not_impl("transform"),
            Operation::Silence { .. } => not_impl("silence"),
            Operation::AddEnchantment { .. } => not_impl("add_enchantment"),
            Operation::RemoveEnchantment { .. } => not_impl("remove_enchantment"),
            Operation::Attack { attacker, defender } => self.op_attack(attacker, defender),
            Operation::Cast { .. } => not_impl("cast"),
            Operation::ChooseOne { .. } => not_impl("choose_one"),
            Operation::Copy { .. } => not_impl("copy"),
            Operation::SetAside { .. } => not_impl("set_aside"),
            Operation::ReturnToZone { .. } => not_impl("return_to_zone"),
        }
    }

    fn op_draw(&mut self, player: PlayerRef, count: u8) -> Result<(), ApplyFail> {
        let players = resolve_player_ref(&self.state, &self.binding, player).map_err(map_select)?;

        // Drawing can cross from real cards into fatigue within one operation.
        // Preflight every player before decrementing a deck or incrementing
        // fatigue so a later immune Hero cannot leave an earlier mutation in
        // the working state. Divine Shield is a Minion damage guard and does
        // not apply to fatigue.
        let mut fatigue_targets = Vec::new();
        for pid in &players {
            let Some(player) = self.state.player(*pid) else {
                return Err(ApplyFail::Unsupported(
                    UnsupportedReason::InsufficientState {
                        what: format!("draw: 玩家 {pid:?} 不存在"),
                    },
                ));
            };
            if usize::from(count) > player.deck_count {
                if let Some(hero) = self.state.hero_entity(*pid) {
                    fatigue_targets.push(hero);
                }
            }
        }
        self.ensure_damage_targets_supported(&fatigue_targets, DamageKind::Fatigue)?;

        for pid in players {
            for _ in 0..count {
                let p = self
                    .state
                    .players
                    .get_mut(&pid)
                    .expect("draw player preflighted");
                if p.deck_count == 0 {
                    p.fatigue = p.fatigue.saturating_add(1);
                    let fatigue = p.fatigue;
                    self.trace.entries.push(TraceEntry {
                        op: "draw".into(),
                        detail: format!("player={} fatigue={fatigue}", pid.0),
                    });
                    apply_damage_to_player(&mut self.state, pid, fatigue);
                } else {
                    p.deck_count -= 1;
                    p.hand_count = p.hand_count.saturating_add(1);
                    self.trace.entries.push(TraceEntry {
                        op: "draw".into(),
                        detail: format!("player={} deck→hand", pid.0),
                    });
                }
            }
        }
        Ok(())
    }

    fn op_attack(&mut self, attacker: &Selector, defender: &Selector) -> Result<(), ApplyFail> {
        let attackers =
            resolve_selector(&self.state, &self.binding, attacker).map_err(map_select)?;
        let defenders =
            resolve_selector(&self.state, &self.binding, defender).map_err(map_select)?;
        let [attacker] = attackers.as_slice() else {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!(
                        "Effect IR Attack requires one attacker, resolved {}",
                        attackers.len()
                    ),
                },
            ));
        };
        let [defender] = defenders.as_slice() else {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!(
                        "Effect IR Attack requires one defender, resolved {}",
                        defenders.len()
                    ),
                },
            ));
        };
        self.apply_combat_damage(*attacker, *defender)
    }

    fn apply_combat_damage(
        &mut self,
        attacker_id: EntityId,
        defender_id: EntityId,
    ) -> Result<(), ApplyFail> {
        let attacker = self.state.entity(attacker_id).ok_or_else(|| {
            ApplyFail::Unsupported(UnsupportedReason::InsufficientState {
                what: format!("combat attacker {} missing", attacker_id.0),
            })
        })?;
        let defender = self.state.entity(defender_id).ok_or_else(|| {
            ApplyFail::Unsupported(UnsupportedReason::InsufficientState {
                what: format!("combat defender {} missing", defender_id.0),
            })
        })?;
        if attacker.zone != Zone::Play || defender.zone != Zone::Play {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::ActionUnavailable {
                    what: "combat participants must both be in play".into(),
                },
            ));
        }
        if !matches!(attacker.card_type, Some(CardType::Hero | CardType::Minion))
            || !matches!(defender.card_type, Some(CardType::Hero | CardType::Minion))
        {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: "combat participant type must be known Hero or Minion".into(),
                },
            ));
        }
        if !attacker.action.attack_known {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!("combat attacker {} attack unknown", attacker_id.0),
                },
            ));
        }
        if defender.card_type == Some(CardType::Minion) && !defender.action.attack_known {
            return Err(ApplyFail::Unsupported(
                UnsupportedReason::InsufficientState {
                    what: format!("combat defender {} attack unknown", defender_id.0),
                },
            ));
        }

        let outgoing = attacker.attack.max(0);
        // 防守英雄不会用武器反击；只有随从防守时造成反伤。
        let retaliation = if defender.card_type == Some(CardType::Minion) {
            defender.attack.max(0)
        } else {
            0
        };
        let defender_detail = apply_damage_to_entity(&mut self.state, defender_id, outgoing);
        let attacker_detail = (retaliation > 0)
            .then(|| apply_damage_to_entity(&mut self.state, attacker_id, retaliation));
        self.trace.entries.push(TraceEntry {
            op: "attack".into(),
            detail: format!(
                "attacker={} defender={} outgoing={} retaliation={}",
                attacker_id.0, defender_id.0, outgoing, retaliation
            ),
        });
        self.trace.entries.push(TraceEntry {
            op: "damage".into(),
            detail: defender_detail,
        });
        if let Some(detail) = attacker_detail {
            self.trace.entries.push(TraceEntry {
                op: "damage".into(),
                detail,
            });
        }
        Ok(())
    }

    fn op_damage(&mut self, target: &Selector, amount: i32) -> Result<(), ApplyFail> {
        let amount = amount.max(0);
        let ids = resolve_selector(&self.state, &self.binding, target).map_err(map_select)?;
        self.ensure_damage_targets_supported(&ids, DamageKind::Effect)?;

        for id in ids {
            let detail = apply_damage_to_entity(&mut self.state, id, amount);
            self.trace.entries.push(TraceEntry {
                op: "damage".into(),
                detail,
            });
        }
        Ok(())
    }

    /// Damage prevention is not modeled yet. Validate the complete target set
    /// before touching any entity so one protected target cannot leave earlier
    /// targets partially mutated in the working state.
    fn ensure_damage_targets_supported(
        &self,
        ids: &[EntityId],
        kind: DamageKind,
    ) -> Result<(), ApplyFail> {
        for id in ids {
            let Some(entity) = self.state.entity(*id) else {
                continue;
            };
            let keyword = if entity.action.immune {
                Some("immune")
            } else if kind.is_blocked_by_divine_shield() && entity.action.divine_shield {
                Some("divine_shield")
            } else {
                None
            };
            if let Some(keyword) = keyword {
                return Err(ApplyFail::Unsupported(UnsupportedReason::NotImplemented {
                    what: format!("{} target {keyword}", kind.label()),
                }));
            }
        }
        Ok(())
    }

    fn op_heal(&mut self, target: &Selector, amount: i32) -> Result<(), ApplyFail> {
        let amount = amount.max(0);
        let ids = resolve_selector(&self.state, &self.binding, target).map_err(map_select)?;
        for id in ids {
            let (healed, is_hero, controller, current) = {
                let Some(entity) = self.state.entities.get_mut(&id) else {
                    continue;
                };
                let before = entity.damage;
                entity.damage = (entity.damage - amount).max(0);
                let healed = before - entity.damage;
                (
                    healed,
                    entity.is_hero(),
                    entity.controller,
                    entity.current_health(),
                )
            };
            if is_hero {
                if let Some(pid) = controller {
                    if let Some(player) = self.state.players.get_mut(&pid) {
                        player.hero_health = current;
                    }
                }
            }
            self.trace.entries.push(TraceEntry {
                op: "heal".into(),
                detail: format!("target={} healed={healed}", id.0),
            });
        }
        Ok(())
    }

    fn op_gain_armor(&mut self, player: PlayerRef, amount: i32) -> Result<(), ApplyFail> {
        let amount = amount.max(0);
        let players = resolve_player_ref(&self.state, &self.binding, player).map_err(map_select)?;
        for pid in players {
            if let Some(p) = self.state.players.get_mut(&pid) {
                p.armor = p.armor.saturating_add(amount);
                self.trace.entries.push(TraceEntry {
                    op: "gain_armor".into(),
                    detail: format!("player={} amount={amount} armor={}", pid.0, p.armor),
                });
            }
        }
        Ok(())
    }

    fn op_summon(
        &mut self,
        player: PlayerRef,
        card: &CardRef,
        count: u8,
        zone: Zone,
        op_name: &str,
    ) -> Result<(), ApplyFail> {
        let players = resolve_player_ref(&self.state, &self.binding, player).map_err(map_select)?;
        let card_id = card_ref_id(card);
        let definition = self.card_definition(&card_id)?;
        for pid in players {
            for _ in 0..count {
                let board_position = if zone == Zone::Play
                    && definition.as_ref().is_some_and(|definition| {
                        matches!(definition.card_type, CardType::Minion | CardType::Location)
                    }) {
                    let occupied = occupied_board_slots(&self.state, pid);
                    if occupied >= MAX_BOARD_SLOTS {
                        return Err(ApplyFail::Unsupported(
                            UnsupportedReason::ActionUnavailable {
                                what: format!("player {} board is full", pid.0),
                            },
                        ));
                    }
                    Some((occupied + 1) as u8)
                } else {
                    None
                };
                let id = self.state.alloc_sim_entity();
                let mut entity =
                    generated_entity(id, card_id.clone(), zone, pid, definition.as_ref());
                entity.action.zone_position = board_position;
                self.state.entities.insert(id, entity);
                adjust_zone_count(&mut self.state, Some(pid), zone, 1);
                let stats = if let Some(definition) = &definition {
                    format!(
                        "type={:?} cost={} attack={} health={} durability={}",
                        definition.card_type,
                        definition.cost,
                        definition.attack,
                        definition.health,
                        definition.durability
                    )
                } else {
                    "stats unknown".into()
                };
                self.trace.entries.push(TraceEntry {
                    op: op_name.into(),
                    detail: format!(
                        "player={} card={card_id} entity={} zone={zone:?} {stats}",
                        pid.0, id.0,
                    ),
                });
            }
        }
        Ok(())
    }

    fn op_destroy(&mut self, target: &Selector) -> Result<(), ApplyFail> {
        let ids = resolve_selector(&self.state, &self.binding, target).map_err(map_select)?;
        for id in ids {
            // 在场实体标记待死,交给死亡结算(这样亡语才会触发);
            // 不在场的(手牌/牌库里被摧毁)没有死亡语义,直接移区。
            let in_play = self.state.entity(id).is_some_and(|e| e.zone == Zone::Play);
            if in_play {
                self.pending_deaths.insert(id);
                self.trace.entries.push(TraceEntry {
                    op: "destroy".into(),
                    detail: format!("target={} -> pending death", id.0),
                });
            } else {
                set_zone(&mut self.state, id, Zone::Graveyard);
                self.trace.entries.push(TraceEntry {
                    op: "destroy".into(),
                    detail: format!("target={} (not in play)", id.0),
                });
            }
        }
        Ok(())
    }

    fn op_move_zone(&mut self, target: &Selector, to: Zone) -> Result<(), ApplyFail> {
        let ids = resolve_selector(&self.state, &self.binding, target).map_err(map_select)?;
        for id in ids {
            set_zone(&mut self.state, id, to);
            self.trace.entries.push(TraceEntry {
                op: "move_zone".into(),
                detail: format!("target={} to={to:?}", id.0),
            });
        }
        Ok(())
    }

    fn op_modify_tag(&mut self, target: &Selector, tag: &str, value: i32) -> Result<(), ApplyFail> {
        let key = tag.to_ascii_uppercase();
        if !matches!(key.as_str(), "ATK" | "HEALTH" | "COST") {
            return Err(ApplyFail::Unsupported(UnsupportedReason::NotImplemented {
                what: format!("modify_tag:{tag}"),
            }));
        }
        let ids = resolve_selector(&self.state, &self.binding, target).map_err(map_select)?;
        for id in ids {
            {
                let Some(entity) = self.state.entities.get_mut(&id) else {
                    continue;
                };
                match key.as_str() {
                    "ATK" => {
                        entity.attack = value;
                        entity.action.attack_known = true;
                    }
                    "HEALTH" => {
                        entity.health = value;
                        entity.action.health_known = true;
                    }
                    "COST" => {
                        entity.cost = value;
                        entity.action.cost_known = true;
                    }
                    _ => unreachable!(),
                }
            }
            if key == "HEALTH" {
                let (is_hero, controller, current, lethal) = match self.state.entities.get(&id) {
                    Some(entity) => (
                        entity.is_hero(),
                        entity.controller,
                        entity.current_health(),
                        entity.damage >= entity.health,
                    ),
                    None => continue,
                };
                if is_hero {
                    if let Some(pid) = controller {
                        if let Some(player) = self.state.players.get_mut(&pid) {
                            player.hero_health = current;
                        }
                    }
                }
                if lethal {
                    set_zone(&mut self.state, id, Zone::Graveyard);
                }
            }
            self.trace.entries.push(TraceEntry {
                op: "modify_tag".into(),
                detail: format!("target={} tag={key} value={value}", id.0),
            });
        }
        Ok(())
    }

    fn op_refresh_mana(&mut self, player: PlayerRef, amount: Option<i32>) -> Result<(), ApplyFail> {
        let players = resolve_player_ref(&self.state, &self.binding, player).map_err(map_select)?;
        for pid in players {
            if let Some(p) = self.state.players.get_mut(&pid) {
                match amount {
                    None => p.mana = p.max_mana,
                    Some(n) => p.mana = (p.mana + n.max(0)).min(p.max_mana),
                }
                self.trace.entries.push(TraceEntry {
                    op: "refresh_mana".into(),
                    detail: format!("player={} mana={}/{}", pid.0, p.mana, p.max_mana),
                });
            }
        }
        Ok(())
    }

    fn op_overload(&mut self, player: PlayerRef, amount: i32) -> Result<(), ApplyFail> {
        let amount = amount.max(0);
        let players = resolve_player_ref(&self.state, &self.binding, player).map_err(map_select)?;
        for pid in players {
            if let Some(p) = self.state.players.get_mut(&pid) {
                p.overloaded = p.overloaded.saturating_add(amount);
                self.trace.entries.push(TraceEntry {
                    op: "overload".into(),
                    detail: format!("player={} overloaded={}", pid.0, p.overloaded),
                });
            }
        }
        Ok(())
    }
}

fn sample_branches(
    template: &ApplyCtx<'_>,
    snapshot: &PoolSnapshot,
    k: usize,
    req: &SimRequest,
    suffix: &[Operation],
    self_id: Option<PlayerId>,
) -> Result<Result<SampleBranches, BranchFail>, SimError> {
    let n = snapshot.candidates.len();
    let mut rng = SplitMix64::new(req.seed);
    let sample_n = req.branch_budget as usize;
    let mut samples = Vec::with_capacity(sample_n);
    let mut evidence = Evidence::default();
    let mut first_failure = None;
    let p = if sample_n == 0 {
        0.0
    } else {
        1.0 / sample_n as f64
    };
    for _ in 0..sample_n {
        let combo = if k == 0 {
            Vec::new()
        } else if k == 1 {
            let weights: Vec<f64> = snapshot
                .candidates
                .iter()
                .map(|card| card.weight.max(0.0))
                .collect();
            vec![sample_weighted(&weights, &mut rng)]
        } else {
            sample_combination(n, k, &mut rng)
        };
        let cards: Vec<WeightedCard> = combo
            .iter()
            .map(|&i| snapshot.candidates[i].clone())
            .collect();
        match finish_branch(template, &cards, suffix, self_id)? {
            Ok(branch) => {
                evidence.merge(branch.evidence);
                samples.push(WeightedOutcome {
                    probability: p,
                    state: branch.state,
                    note: Some(note_for(&cards)),
                });
            }
            Err(fail) => {
                evidence.merge(fail.evidence);
                if first_failure.is_none() {
                    first_failure = Some(fail.reason);
                }
            }
        }
    }
    if let Some(reason) = first_failure {
        return Ok(Err(BranchFail { reason, evidence }));
    }
    Ok(Ok(SampleBranches { samples, evidence }))
}

struct SampleBranches {
    samples: Vec<WeightedOutcome>,
    evidence: Evidence,
}

struct BranchResult {
    state: SimGameState,
    evidence: Evidence,
}

struct BranchFail {
    reason: UnsupportedReason,
    evidence: Evidence,
}

fn finish_branch(
    template: &ApplyCtx<'_>,
    cards: &[WeightedCard],
    suffix: &[Operation],
    self_id: Option<PlayerId>,
) -> Result<Result<BranchResult, BranchFail>, SimError> {
    let mut branch = ApplyCtx {
        state: template.state.clone(),
        binding: template.binding,
        pools: template.pools,
        rules: template.rules,
        trace: RuleTrace::default(),
        evidence: template.evidence.clone(),
        card_lookup: template.card_lookup,
        build: template.build,
        strict_indirect_rules: template.strict_indirect_rules,
        pending_deaths: std::collections::BTreeSet::new(),
        in_deathrattle: false,
    };
    if let Some(pid) = self_id {
        for card in cards {
            let definition = match branch.card_definition(&card.card_id) {
                Ok(definition) => definition,
                Err(ApplyFail::Unsupported(reason)) => {
                    return Ok(Err(BranchFail {
                        reason,
                        evidence: branch.evidence,
                    }));
                }
                Err(ApplyFail::Error(err)) => return Err(err),
            };
            let id = branch.state.alloc_sim_entity();
            branch.state.entities.insert(
                id,
                generated_entity(
                    id,
                    card.card_id.clone(),
                    Zone::Hand,
                    pid,
                    definition.as_ref(),
                ),
            );
            adjust_zone_count(&mut branch.state, Some(pid), Zone::Hand, 1);
        }
    }
    match branch.run_ops(suffix) {
        Ok(()) => Ok(Ok(BranchResult {
            state: branch.state,
            evidence: branch.evidence,
        })),
        Err(ApplyFail::Unsupported(reason)) => Ok(Err(BranchFail {
            reason,
            evidence: branch.evidence,
        })),
        Err(ApplyFail::Error(err)) => Err(err),
    }
}

fn combination_weights(candidates: &[WeightedCard], combos: &[Vec<usize>]) -> Vec<f64> {
    combos
        .iter()
        .map(|combo| {
            if combo.is_empty() {
                1.0
            } else {
                combo
                    .iter()
                    .map(|&i| candidates[i].weight.max(0.0))
                    .product()
            }
        })
        .collect()
}

fn note_for(cards: &[WeightedCard]) -> String {
    cards
        .iter()
        .map(|c| c.card_id.to_string())
        .collect::<Vec<_>>()
        .join("+")
}

fn validate_and_consume_attack(
    state: &mut SimGameState,
    friendly: PlayerId,
    attacker_id: EntityId,
    defender_id: EntityId,
) -> Result<Option<EntityId>, UnsupportedReason> {
    let attacker =
        state
            .entity(attacker_id)
            .cloned()
            .ok_or_else(|| UnsupportedReason::InsufficientState {
                what: format!("攻击者 {} 不存在", attacker_id.0),
            })?;
    let defender =
        state
            .entity(defender_id)
            .cloned()
            .ok_or_else(|| UnsupportedReason::InsufficientState {
                what: format!("防守者 {} 不存在", defender_id.0),
            })?;
    if attacker.controller != Some(friendly) {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("攻击者 {} 不属于己方", attacker_id.0),
        });
    }
    if defender.controller.is_none() || defender.controller == Some(friendly) {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("防守者 {} 不是敌方实体", defender_id.0),
        });
    }
    if attacker.zone != Zone::Play || defender.zone != Zone::Play {
        return Err(UnsupportedReason::ActionUnavailable {
            what: "攻击双方必须都在场上".into(),
        });
    }
    if !matches!(attacker.card_type, Some(CardType::Hero | CardType::Minion)) {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("攻击者 {} 类型不是已知 Hero/Minion", attacker_id.0),
        });
    }
    if !matches!(defender.card_type, Some(CardType::Hero | CardType::Minion)) {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("防守者 {} 类型不是已知 Hero/Minion", defender_id.0),
        });
    }
    if !attacker.action.attack_known || !attacker.action.health_known {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("攻击者 {} 攻击或生命未观测", attacker_id.0),
        });
    }
    if !defender.action.health_known
        || (defender.card_type == Some(CardType::Minion) && !defender.action.attack_known)
    {
        return Err(UnsupportedReason::InsufficientState {
            what: format!("防守者 {} 战斗数值未观测", defender_id.0),
        });
    }
    if attacker.attack <= 0 {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("攻击者 {} 没有攻击力", attacker_id.0),
        });
    }
    if attacker.current_health() <= 0 || defender.current_health() <= 0 {
        return Err(UnsupportedReason::ActionUnavailable {
            what: "死亡角色不能参与攻击".into(),
        });
    }
    if attacker.action.exhausted || attacker.action.attacks_this_turn > 0 {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("攻击者 {} 本回合已疲劳", attacker_id.0),
        });
    }
    if attacker.action.frozen || attacker.action.cant_attack {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("攻击者 {} 当前不能攻击", attacker_id.0),
        });
    }
    if defender.action.stealth || defender.action.cant_be_attacked {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("防守者 {} 当前不可被攻击", defender_id.0),
        });
    }
    if attacker.action.cannot_attack_heroes && defender.card_type == Some(CardType::Hero) {
        return Err(UnsupportedReason::ActionUnavailable {
            what: format!("攻击者 {} 不能攻击英雄", attacker_id.0),
        });
    }
    if let Some(keyword) =
        unsupported_combat_keyword(&attacker).or_else(|| unsupported_combat_keyword(&defender))
    {
        return Err(UnsupportedReason::NotImplemented {
            what: format!("combat keyword {keyword}"),
        });
    }

    let defender_controller = defender.controller.expect("checked opponent");
    let taunt_exists = state.entities.values().any(|entity| {
        entity.controller == Some(defender_controller)
            && entity.zone == Zone::Play
            && entity.card_type == Some(CardType::Minion)
            && entity.action.taunt
            && !entity.action.stealth
    });
    if taunt_exists && !(defender.card_type == Some(CardType::Minion) && defender.action.taunt) {
        return Err(UnsupportedReason::ActionUnavailable {
            what: "敌方有嘲讽随从，必须先攻击嘲讽".into(),
        });
    }

    let mut weapon_to_break = None;
    if attacker.card_type == Some(CardType::Hero) {
        let weapons = state
            .entities
            .values()
            .filter(|entity| {
                entity.controller == Some(friendly)
                    && entity.zone == Zone::Play
                    && entity.card_type == Some(CardType::Weapon)
            })
            .map(|entity| entity.entity_id)
            .collect::<Vec<_>>();
        if weapons.len() > 1 {
            return Err(UnsupportedReason::InsufficientState {
                what: "己方同时存在多把在场武器".into(),
            });
        }
        if let Some(weapon_id) = weapons.first().copied() {
            let weapon = state.entity(weapon_id).expect("collected weapon");
            if let Some(keyword) = unsupported_combat_keyword(weapon) {
                return Err(UnsupportedReason::NotImplemented {
                    what: format!("weapon combat keyword {keyword}"),
                });
            }
            if !weapon.action.durability_known {
                return Err(UnsupportedReason::InsufficientState {
                    what: format!("武器 {} 耐久未观测", weapon_id.0),
                });
            }
            if weapon.action.durability <= 0 {
                return Err(UnsupportedReason::ActionUnavailable {
                    what: format!("武器 {} 已无耐久", weapon_id.0),
                });
            }
            let weapon = state
                .entities
                .get_mut(&weapon_id)
                .expect("collected weapon");
            weapon.action.durability -= 1;
            if weapon.action.durability == 0 {
                weapon_to_break = Some(weapon_id);
            }
        }
    }

    let attacker = state
        .entities
        .get_mut(&attacker_id)
        .expect("validated attacker");
    attacker.action.attacks_this_turn = attacker.action.attacks_this_turn.saturating_add(1);
    attacker.action.exhausted = true;
    Ok(weapon_to_break)
}

fn unsupported_combat_keyword(entity: &crate::state::SimEntity) -> Option<&'static str> {
    let action = &entity.action;
    if action.windfury {
        Some("windfury")
    } else if action.mega_windfury {
        Some("mega_windfury")
    } else if action.divine_shield {
        Some("divine_shield")
    } else if action.poisonous {
        Some("poisonous")
    } else if action.lifesteal {
        Some("lifesteal")
    } else if action.immune {
        Some("immune")
    } else if action.immune_while_attacking {
        Some("immune_while_attacking")
    } else {
        None
    }
}

fn apply_damage_to_entity(state: &mut SimGameState, id: EntityId, amount: i32) -> String {
    let Some(entity) = state.entities.get(&id) else {
        return format!("target={} missing", id.0);
    };
    let is_hero = entity.is_hero();
    let controller = entity.controller;
    let mut absorbed = 0;
    let mut remaining = amount;
    if is_hero {
        if let Some(pid) = controller {
            if let Some(player) = state.players.get_mut(&pid) {
                absorbed = player.armor.min(remaining);
                player.armor -= absorbed;
                remaining -= absorbed;
            }
        }
    }
    let (lethal, current) = {
        let Some(entity) = state.entities.get_mut(&id) else {
            return format!("target={} missing", id.0);
        };
        entity.damage = entity.damage.saturating_add(remaining);
        (entity.damage >= entity.health, entity.current_health())
    };
    if is_hero {
        if let Some(pid) = controller {
            if let Some(player) = state.players.get_mut(&pid) {
                player.hero_health = current;
            }
        }
    }
    // 不在此移区:D1 要求整个 Operation 跑完后由死亡结算阶段统一处理
    // (真实日志 42/42 个 DEATHS 块都不含伤害变化,死亡另起块)。
    format!(
        "target={} amount={amount} armor_absorbed={absorbed} lethal={lethal}",
        id.0
    )
}

fn apply_damage_to_player(state: &mut SimGameState, player: PlayerId, amount: i32) {
    if let Some(hero_id) = state.hero_entity(player) {
        apply_damage_to_entity(state, hero_id, amount);
        return;
    }
    // 无英雄实体时只改玩家投影,不捏造英雄。
    if let Some(p) = state.players.get_mut(&player) {
        let absorbed = p.armor.min(amount);
        p.armor -= absorbed;
        p.hero_health = (p.hero_health - (amount - absorbed)).max(0);
    }
}

fn generated_entity(
    entity_id: EntityId,
    card_id: CardId,
    zone: Zone,
    controller: PlayerId,
    definition: Option<&SimCardDefinition>,
) -> crate::state::SimEntity {
    let mut action = crate::state::SimEntityActionState::default();
    let (card_type, cost, attack, health) = match definition {
        Some(definition) => {
            action.cost_known = true;
            action.attack_known = true;
            action.health_known = true;
            action.durability = definition.durability;
            action.durability_known = true;
            if zone == Zone::Play && definition.card_type == CardType::Minion {
                // SimCardDefinition 不携带 Charge/Rush；严格路径不能把刚召唤的
                // token 猜成可立即攻击，先保守保留召唤疲劳。
                action.exhausted = true;
            }
            (
                Some(definition.card_type),
                definition.cost,
                definition.attack,
                definition.health,
            )
        }
        None => (None, 0, 0, 0),
    };
    crate::state::SimEntity {
        entity_id,
        card_id: Some(card_id),
        card_type,
        zone,
        controller: Some(controller),
        attack,
        health,
        damage: 0,
        cost,
        action,
        simulated: true,
    }
}

fn occupied_board_slots(state: &SimGameState, controller: PlayerId) -> usize {
    state
        .entities
        .values()
        .filter(|entity| {
            entity.controller == Some(controller)
                && entity.zone == Zone::Play
                && matches!(
                    entity.card_type,
                    Some(CardType::Minion | CardType::Location)
                )
        })
        .count()
}

pub(crate) fn set_zone(state: &mut SimGameState, id: EntityId, to: Zone) {
    let Some(entity) = state.entities.get(&id) else {
        return;
    };
    let from = entity.zone;
    if from == to {
        return;
    }
    let controller = entity.controller;
    let old_position = entity.action.zone_position;
    let weapon_attack = (entity.card_type == Some(CardType::Weapon))
        .then_some((entity.attack.max(0), entity.action.attack_known));
    if let Some(entity) = state.entities.get_mut(&id) {
        entity.zone = to;
        if to != Zone::Play {
            entity.action.zone_position = None;
        }
    }
    if from == Zone::Play && to != Zone::Play {
        if let (Some(controller), Some(old_position)) = (controller, old_position) {
            for entity in state.entities.values_mut() {
                if entity.entity_id != id
                    && entity.controller == Some(controller)
                    && entity.zone == Zone::Play
                    && entity
                        .action
                        .zone_position
                        .is_some_and(|position| position > old_position)
                {
                    entity.action.zone_position = entity
                        .action
                        .zone_position
                        .map(|position| position.saturating_sub(1));
                }
            }
        }
    }
    if let (Some(controller), Some((attack, attack_known))) = (controller, weapon_attack) {
        if from != Zone::Play && to == Zone::Play {
            adjust_hero_attack_for_weapon(state, controller, attack, attack_known, true);
        } else if from == Zone::Play && to != Zone::Play {
            adjust_hero_attack_for_weapon(state, controller, attack, attack_known, false);
        }
    }
    adjust_zone_count(state, controller, from, -1);
    adjust_zone_count(state, controller, to, 1);
}

fn adjust_hero_attack_for_weapon(
    state: &mut SimGameState,
    controller: PlayerId,
    weapon_attack: i32,
    weapon_attack_known: bool,
    entering_play: bool,
) {
    let Some(hero_id) = state.hero_entity(controller) else {
        return;
    };
    let Some(hero) = state.entities.get_mut(&hero_id) else {
        return;
    };
    hero.attack = if entering_play {
        hero.attack.saturating_add(weapon_attack)
    } else {
        hero.attack.saturating_sub(weapon_attack).max(0)
    };
    hero.action.attack_known &= weapon_attack_known;
}

fn adjust_zone_count(
    state: &mut SimGameState,
    controller: Option<PlayerId>,
    zone: Zone,
    delta: i32,
) {
    let Some(pid) = controller else {
        return;
    };
    let Some(player) = state.players.get_mut(&pid) else {
        return;
    };
    match zone {
        Zone::Hand => {
            player.hand_count = (player.hand_count as i32 + delta).max(0) as usize;
        }
        Zone::Deck => {
            player.deck_count = (player.deck_count as i32 + delta).max(0) as usize;
        }
        _ => {}
    }
}

fn card_ref_id(card: &CardRef) -> CardId {
    match card {
        CardRef::Id(id) => id.clone(),
        CardRef::Dbf(dbf) => CardId::new(format!("dbf:{}", dbf.0)),
    }
}

fn map_select(err: SelectError) -> ApplyFail {
    match err {
        SelectError::Binding(msg) => ApplyFail::Error(SimError::InvalidBinding(msg)),
        SelectError::Insufficient(what) => {
            ApplyFail::Unsupported(UnsupportedReason::InsufficientState { what })
        }
    }
}

fn not_impl(what: &str) -> Result<(), ApplyFail> {
    Err(ApplyFail::Unsupported(UnsupportedReason::NotImplemented {
        what: what.into(),
    }))
}

fn is_random_op(op: &Operation) -> bool {
    matches!(
        op,
        Operation::Discover { .. } | Operation::RandomChoice { .. }
    )
}

fn count_random_ops(ops: &[Operation]) -> usize {
    ops.iter().map(count_random_in).sum()
}

fn count_random_in(op: &Operation) -> usize {
    match op {
        Operation::Discover { .. } | Operation::RandomChoice { .. } => 1,
        Operation::Repeat { effects, .. } | Operation::ForEach { effects, .. } => {
            count_random_ops(effects)
        }
        Operation::Conditional {
            then, otherwise, ..
        } => count_random_ops(then) + count_random_ops(otherwise),
        Operation::ChooseOne { options } => options.iter().map(|ops| count_random_ops(ops)).sum(),
        _ => 0,
    }
}

fn has_nested_random(ops: &[Operation]) -> bool {
    ops.iter().any(|op| match op {
        Operation::Repeat { effects, .. } | Operation::ForEach { effects, .. } => {
            count_random_ops(effects) > 0
        }
        Operation::Conditional {
            then, otherwise, ..
        } => count_random_ops(then) + count_random_ops(otherwise) > 0,
        Operation::ChooseOne { options } => options.iter().any(|ops| count_random_ops(ops) > 0),
        _ => false,
    })
}

fn format_rule_version(program: &EffectProgram) -> String {
    match program.build_range.max {
        Some(max) => format!(
            "{}@{}..{}",
            program.card_id, program.build_range.min.0, max.0
        ),
        None => format!("{}@{}..", program.card_id, program.build_range.min.0),
    }
}

fn action_meta(name: &str, req: &SimRequest) -> ReproMeta {
    ReproMeta {
        seed: req.seed,
        branch_budget: req.branch_budget,
        method: SampleMethod::Exact,
        truncation: None,
        chance_kind: None,
        rule_version: format!("action:{name}"),
        pool_snapshot_hashes: Vec::new(),
    }
}

fn action_unsupported(name: &str, reason: UnsupportedReason, req: &SimRequest) -> SimOutcome {
    unsupported(
        OutcomeSet::Unsupported(reason),
        RuleTrace::default(),
        action_meta(name, req),
        Evidence::default(),
    )
}

fn finish_outcome(
    outcome: OutcomeSet,
    trace: RuleTrace,
    mut meta: ReproMeta,
    evidence: Evidence,
) -> SimOutcome {
    let Evidence {
        pool_hashes,
        unknown_rule_cards,
        rule_uses,
    } = evidence;
    meta.pool_snapshot_hashes = pool_hashes.into_iter().collect();
    SimOutcome {
        outcome,
        trace,
        meta,
        unknown_rule_cards: unknown_rule_cards.into_iter().collect(),
        rule_uses: rule_uses.into_values().collect(),
    }
}

fn unsupported(
    outcome: OutcomeSet,
    trace: RuleTrace,
    meta: ReproMeta,
    evidence: Evidence,
) -> SimOutcome {
    finish_outcome(outcome, trace, meta, evidence)
}
