//! Pool/Rule Inspector 页的 headless 自查(M3 验收最后一项的可重复版本)。
//!
//! 跑的是 Pools 页的**真实数据通路**:同一个 `rules_store::RulesStore::load_default()`
//! 加载器(经 `#[path]` 直接编译生产源码,不是复制品)、同一个
//! `pool_query::SqliteCardQuery` 适配器、同一套 PoolContext 构造与试执行调用
//! (与 `pages/pools.rs` 的 `build_context` / `apply_rule` / `StoreResolver` 逐行对应),
//! 只是把 GPUI 渲染换成 stdout。因此它能覆盖真机自查里除「像素画出来」以外的全部风险:
//! 真实 cards.sqlite 上的池求值、Trace/Hash/exact、规则试执行与 Unsupported 显式报告。
//!
//! 用法:`cargo run -p desktop --example pools_selfcheck`
//! 退出码 0 = 全绿;非 0 = 有池求值失败 / 试执行 Err / 变形术未显式 Unsupported。

#[allow(dead_code)]
#[path = "../src/pool_query.rs"]
mod pool_query;
#[allow(dead_code)]
#[path = "../src/rules_store.rs"]
mod rules_store;

use hs_pools::{PoolContext, PoolError, PoolSnapshot, PoolSpec};
use hs_sim::{
    apply_program, OutcomeSet, PoolResolver, SimError, SimGameState, SimRequest, TargetBinding,
};
use hs_types::{CardId, FormatType, GameMode, PoolId};
use rules_store::RulesStore;

/// 与 `pages/pools.rs` 同名常量保持一致(候选渲染限行)。
const MAX_CANDIDATE_ROWS: usize = 200;

/// 页面职业 chips 全集(`pages/pools.rs::CLASSES`)。
const CLASSES: [&str; 11] = [
    "DEATHKNIGHT",
    "DEMONHUNTER",
    "DRUID",
    "HUNTER",
    "MAGE",
    "PALADIN",
    "PRIEST",
    "ROGUE",
    "SHAMAN",
    "WARLOCK",
    "WARRIOR",
];

/// 「变形术」——设计上必须显式 Unsupported 的活例(design §4 之外的 Transform)。
const POLYMORPH: &str = "CS2_022";

/// 对齐 `pages/pools.rs::PoolsView::build_context`(不用 Live 状态,revision=0)。
fn build_context(
    store: &RulesStore,
    spec: &PoolSpec,
    format: FormatType,
    player_class: &str,
) -> PoolContext {
    PoolContext {
        build_id: store.current_build.unwrap_or(spec.build_range.min),
        game_mode: GameMode::Ranked,
        format,
        player_class: vec![player_class.to_string()],
        original_class: Vec::new(),
        source_card: spec.source_card_id.clone(),
        source_entity: None,
        current_cost: None,
        state_revision: 0,
    }
}

/// 对齐 `pages/pools.rs::StoreResolver`(试执行时的池回调)。
struct StoreResolver<'a> {
    store: &'a RulesStore,
    format: FormatType,
    player_class: String,
    spec_ctx_source: CardId,
}

impl PoolResolver for StoreResolver<'_> {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        let spec = self
            .store
            .pools
            .iter()
            .find(|s| &s.pool_id == pool_id)
            .ok_or_else(|| {
                SimError::Pool(hs_pools::PoolError::Invalid(format!("池 {pool_id} 未加载")))
            })?;
        let cards =
            self.store.card_query.clone().ok_or_else(|| {
                SimError::Pool(hs_pools::PoolError::Invalid("卡牌库未就绪".into()))
            })?;
        let mut ctx = build_context(self.store, spec, self.format, &self.player_class);
        ctx.source_card = self.spec_ctx_source.clone();
        hs_pools::resolve(spec, &ctx, cards.as_ref(), None).map_err(SimError::Pool)
    }
}

/// 对齐 `pages/pools.rs::summarize_outcome`。
fn summarize_outcome(outcome: &OutcomeSet) -> String {
    match outcome {
        OutcomeSet::Deterministic(_) => "Deterministic".into(),
        OutcomeSet::Exact(branches) => format!("Exact({} 分支)", branches.len()),
        OutcomeSet::Lazy(_) => "Lazy(M3 未实现)".into(),
        OutcomeSet::Sampled(s) => format!("Sampled({} 样本)", s.samples.len()),
        OutcomeSet::Unsupported(reason) => format!("Unsupported({reason:?})"),
    }
}

fn main() {
    let store = RulesStore::load_default();
    let mut failures: Vec<String> = Vec::new();

    println!("== 加载 ==");
    println!("rules_dir     : {}", store.rules_dir.display());
    println!("卡牌库 build  : {:?}", store.current_build);
    println!("池定义        : {} 个", store.pools.len());
    println!("规则          : {} 张卡", store.rule_set.len());
    for err in &store.errors {
        println!("加载错误      : {err}");
        failures.push(format!("加载错误: {err}"));
    }
    if store.card_query.is_none() {
        println!("\n卡牌库未就绪,池求值无法自查。");
        std::process::exit(2);
    }
    let cards = store.card_query.clone().expect("已判空");

    // ---- 池求值:每个池 × 每个职业 chip,标准模式(页面默认 format) ----
    println!("\n== 池求值(Standard × 11 职业)==");
    let mut max_candidates = 0usize;
    for spec in &store.pools {
        let mut counts: Vec<usize> = Vec::new();
        let mut sample: Option<PoolSnapshot> = None;
        let mut needs_live = false;
        for class in CLASSES {
            let ctx = build_context(&store, spec, FormatType::Standard, class);
            match hs_pools::resolve(spec, &ctx, cards.as_ref(), None) {
                Ok(snapshot) => {
                    counts.push(snapshot.candidates.len());
                    max_candidates = max_candidates.max(snapshot.candidates.len());
                    if sample.is_none() {
                        sample = Some(snapshot);
                    }
                }
                // 动态池在「用 Live 状态」关闭时要求快照 = 设计如此,页面照原样展示该错误。
                Err(PoolError::StateRequired { .. }) => needs_live = true,
                Err(err) => failures.push(format!("池 {} [{class}] 求值失败: {err}", spec.pool_id)),
            }
        }
        let Some(snapshot) = sample else {
            if needs_live {
                println!(
                    "{:<38} 需 Live 状态(动态池,设计如此)",
                    spec.pool_id.to_string()
                );
            }
            continue;
        };
        let (lo, hi) = (
            counts.iter().copied().min().unwrap_or(0),
            counts.iter().copied().max().unwrap_or(0),
        );
        println!(
            "{:<38} 候选 {lo}–{hi}  trace {} 级  exact={}  hash={}",
            spec.pool_id.to_string(),
            snapshot.trace.len(),
            snapshot.exact,
            snapshot.context_hash.chars().take(16).collect::<String>(),
        );
        if snapshot.trace.is_empty() {
            failures.push(format!("池 {} 无逐级过滤 Trace", spec.pool_id));
        }
    }
    println!(
        "\n最大候选数 {max_candidates},页面限行 {MAX_CANDIDATE_ROWS} → 限行{}生效",
        if max_candidates > MAX_CANDIDATE_ROWS {
            ""
        } else {
            "未被触发(候选未超限),但"
        }
    );

    // ---- 规则试执行:与页面「点规则」同一条路径 ----
    println!("\n== 规则试执行(空沙盘,与页面 use_live_state=false 一致)==");
    let Some(build) = store.current_build else {
        println!("卡牌库缺 build,跳过试执行。");
        std::process::exit(2);
    };
    let state = SimGameState::default();
    let mut card_ids: Vec<CardId> = store.rule_set.iter().map(|(id, _)| id.clone()).collect();
    card_ids.sort();
    let mut polymorph_seen = false;
    let mut needs_target: Vec<String> = Vec::new();
    let mut empty_trace: Vec<String> = Vec::new();
    for card_id in &card_ids {
        let Some(program) = store.rule_set.select(build, card_id) else {
            failures.push(format!("{card_id}: build {} 下无生效规则版本", build.0));
            continue;
        };
        let resolver = StoreResolver {
            store: &store,
            format: FormatType::Standard,
            player_class: "MAGE".into(),
            spec_ctx_source: program.card_id.clone(),
        };
        match apply_program(
            &state,
            program,
            &TargetBinding::default(),
            &resolver,
            &SimRequest::default(),
        ) {
            Ok(result) => {
                let summary = summarize_outcome(&result.outcome);
                println!(
                    "{:<20} status={:<8} {summary}  (trace {} 步)",
                    card_id.to_string(),
                    format!("{:?}", program.status),
                    result.trace.entries.len(),
                );
                if result.trace.entries.is_empty()
                    && matches!(result.outcome, OutcomeSet::Deterministic(_))
                {
                    empty_trace.push(card_id.to_string());
                }
                if card_id.as_str() == POLYMORPH {
                    polymorph_seen = true;
                    if !matches!(result.outcome, OutcomeSet::Unsupported(_)) {
                        failures.push(format!("变形术 {POLYMORPH} 未显式 Unsupported: {summary}"));
                    }
                }
            }
            // 页面用 TargetBinding::default() 且无目标绑定入口 → 需目标的卡必然走这条。
            // 设计如此(执行器拒绝空绑定),页面展示「执行失败」文案,不是崩溃。
            Err(SimError::InvalidBinding(_)) => {
                needs_target.push(card_id.to_string());
                println!(
                    "{:<20} status={:<8} 需目标绑定(页面无绑定入口)",
                    card_id.to_string(),
                    format!("{:?}", program.status),
                );
            }
            Err(SimError::Pool(PoolError::StateRequired { .. })) => {
                println!(
                    "{:<20} status={:<8} 需 Live 状态(动态池)",
                    card_id.to_string(),
                    format!("{:?}", program.status),
                );
            }
            Err(err) => failures.push(format!("{card_id}: 试执行 Err(非预期): {err}")),
        }
    }
    if !polymorph_seen {
        failures.push(format!("未执行到变形术 {POLYMORPH}(规则缺失?)"));
    }

    // ---- 结论 ----
    println!("\n== 页面可用性备注(非失败,记账用)==");
    println!(
        "需目标绑定才能试执行的卡 {} 张:{}",
        needs_target.len(),
        needs_target.join(" ")
    );
    println!(
        "Deterministic 且 trace 为空的卡 {} 张(无 play 也无带效果的 triggers,空 trace 正确):{}",
        empty_trace.len(),
        empty_trace.join(" ")
    );

    println!("\n== 结论 ==");
    if failures.is_empty() {
        println!(
            "全绿:{} 个池、{} 张卡走完真实通路,无非预期错误,变形术显式 Unsupported。",
            store.pools.len(),
            card_ids.len()
        );
        println!("(GPUI 绘制本身不在本自查范围,需真机开窗。)");
    } else {
        for f in &failures {
            println!("FAIL: {f}");
        }
        std::process::exit(1);
    }
}
