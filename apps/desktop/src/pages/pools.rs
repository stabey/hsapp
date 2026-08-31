//! Pool/Rule Inspector 页(M3 Track D,设计 §15.4 / design.md §6)。
//!
//! 输入:已加载池列表(点击求值)+ 格式/职业 chips + 「用 Live 状态」开关;
//! 输出:候选(中文名+权重,限行)/逐级过滤 Trace/Snapshot Hash/exact;
//! 规则侧:RuleSet 列表(status 徽章)+ 对沙盘试执行(OutcomeSet + RuleTrace)。
//! 渲染限行沿用 af487c3 的策略:大集合先渲染前 N 行 + 计数提示。

use gpui::{div, prelude::*, Context, Window};
use hs_pools::{PoolContext, PoolSnapshot, PoolSpec};
use hs_sim::{
    apply_program, OutcomeSet, PoolResolver, SimError, SimGameState, SimRequest, TargetBinding,
};
use hs_types::{CardId, FormatType, GameMode, PoolId};
use hs_ui::components as c;
use hs_ui::tokens as t;

use crate::rules_store::RulesStore;
use crate::session::SessionHandle;

/// 候选列表渲染上限(af487c3 同策略,防大候选集卡死)。
const MAX_CANDIDATE_ROWS: usize = 200;
/// Trace/执行轨迹渲染上限。
const MAX_TRACE_ROWS: usize = 100;

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

pub struct PoolsView {
    format: FormatType,
    player_class: String,
    use_live_state: bool,
    selected_pool: Option<PoolId>,
    snapshot: Option<Result<PoolSnapshot, String>>,
    /// 最近一次规则试执行:(card_id, 结果摘要, 轨迹行)。
    apply_result: Option<(CardId, String, Vec<String>)>,
}

impl PoolsView {
    pub fn new() -> Self {
        Self {
            format: FormatType::Standard,
            player_class: "MAGE".into(),
            use_live_state: false,
            selected_pool: None,
            snapshot: None,
            apply_result: None,
        }
    }

    fn build_context(&self, spec: &PoolSpec, cx: &gpui::App) -> PoolContext {
        let store = cx.global::<RulesStore>();
        let state_revision = if self.use_live_state {
            cx.try_global::<SessionHandle>()
                .map(|handle| handle.0.read(cx).state.revision)
                .unwrap_or(0)
        } else {
            0
        };
        PoolContext {
            build_id: store.current_build.unwrap_or(spec.build_range.min),
            game_mode: GameMode::Ranked,
            format: self.format,
            player_class: vec![self.player_class.clone()],
            original_class: Vec::new(),
            source_card: spec.source_card_id.clone(),
            source_entity: None,
            current_cost: None,
            state_revision,
        }
    }

    fn resolve_pool(&mut self, pool_id: PoolId, cx: &mut Context<Self>) {
        self.selected_pool = Some(pool_id.clone());
        let store = cx.global::<RulesStore>();
        let Some(spec) = store.pools.iter().find(|s| s.pool_id == pool_id).cloned() else {
            self.snapshot = Some(Err(format!("池 {pool_id} 未加载")));
            return;
        };
        let Some(cards) = store.card_query.clone() else {
            self.snapshot = Some(Err("卡牌库未就绪,无法求值".into()));
            return;
        };
        let ctx = self.build_context(&spec, cx);
        let result = if self.use_live_state {
            match cx.try_global::<SessionHandle>() {
                Some(handle) => {
                    let session = handle.0.read(cx);
                    hs_pools::resolve(&spec, &ctx, cards.as_ref(), Some(&session.state))
                }
                None => hs_pools::resolve(&spec, &ctx, cards.as_ref(), None),
            }
        } else {
            hs_pools::resolve(&spec, &ctx, cards.as_ref(), None)
        };
        self.snapshot = Some(result.map_err(|e| e.to_string()));
    }

    /// 对沙盘试执行规则:Live 有状态用 from_observed,否则空沙盘。
    fn apply_rule(&mut self, card_id: CardId, cx: &mut Context<Self>) {
        let store = cx.global::<RulesStore>();
        let Some(build) = store.current_build else {
            self.apply_result = Some((card_id, "卡牌库未就绪(缺 build)".into(), Vec::new()));
            return;
        };
        let Some(program) = store.rule_set.select(build, &card_id).cloned() else {
            self.apply_result = Some((card_id, "该 build 下无生效规则版本".into(), Vec::new()));
            return;
        };
        let state = match cx.try_global::<SessionHandle>() {
            Some(handle) if self.use_live_state => {
                SimGameState::from_observed(&handle.0.read(cx).state)
            }
            _ => SimGameState::default(),
        };
        let outcome = {
            let resolver = StoreResolver {
                view: &*self,
                cx,
                spec_ctx_source: program.card_id.clone(),
            };
            apply_program(
                &state,
                &program,
                &TargetBinding::default(),
                &resolver,
                &SimRequest::default(),
            )
        };
        self.apply_result = Some(match outcome {
            Ok(result) => {
                let summary = summarize_outcome(&result.outcome);
                let mut lines: Vec<String> = result
                    .trace
                    .entries
                    .iter()
                    .map(|e| format!("{}: {}", e.op, e.detail))
                    .collect();
                lines.push(format!(
                    "meta: seed={} budget={} method={:?}",
                    result.meta.seed, result.meta.branch_budget, result.meta.method
                ));
                (card_id, summary, lines)
            }
            Err(err) => (card_id, format!("执行失败: {err}"), Vec::new()),
        });
    }
}

/// 试执行用的池回调:用页面当前 chips 组上下文,经全局 store 求值。
struct StoreResolver<'a, 'b> {
    view: &'a PoolsView,
    cx: &'a Context<'b, PoolsView>,
    spec_ctx_source: CardId,
}

impl PoolResolver for StoreResolver<'_, '_> {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        let store = self.cx.global::<RulesStore>();
        let spec = store
            .pools
            .iter()
            .find(|s| &s.pool_id == pool_id)
            .ok_or_else(|| {
                SimError::Pool(hs_pools::PoolError::Invalid(format!("池 {pool_id} 未加载")))
            })?;
        let cards = store
            .card_query
            .clone()
            .ok_or_else(|| SimError::Pool(hs_pools::PoolError::Invalid("卡牌库未就绪".into())))?;
        let mut ctx = self.view.build_context(spec, self.cx);
        ctx.source_card = self.spec_ctx_source.clone();
        let state_ref = self
            .cx
            .try_global::<SessionHandle>()
            .filter(|_| self.view.use_live_state)
            .map(|handle| &handle.0.read(self.cx).state);
        hs_pools::resolve(spec, &ctx, cards.as_ref(), state_ref).map_err(SimError::Pool)
    }
}

fn summarize_outcome(outcome: &OutcomeSet) -> String {
    match outcome {
        OutcomeSet::Deterministic(_) => "Deterministic(唯一结果)".into(),
        OutcomeSet::Exact(branches) => format!("Exact({} 个分支,精确概率)", branches.len()),
        OutcomeSet::Lazy(_) => "Lazy(惰性,M3 未实现)".into(),
        OutcomeSet::Sampled(s) => format!(
            "Sampled({} 个样本,{:?}{})",
            s.samples.len(),
            s.method,
            s.total_branches
                .map(|n| format!(",理论分支 {n}"))
                .unwrap_or_default()
        ),
        OutcomeSet::Unsupported(reason) => format!("Unsupported: {reason:?}"),
    }
}

fn chip(
    id: gpui::SharedString,
    label: impl Into<gpui::SharedString>,
    active: bool,
    on_click: impl Fn(&mut PoolsView, &mut Context<PoolsView>) + 'static,
    cx: &mut Context<PoolsView>,
) -> impl IntoElement {
    div()
        .id(id)
        .px(t::space_2())
        .py(t::space_1())
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .when(active, |el| el.bg(t::bg_active()).text_color(t::accent()))
        .when(!active, |el| {
            el.text_color(t::text_muted())
                .hover(|el| el.bg(t::bg_hover()))
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            on_click(this, cx);
            cx.notify();
        }))
        .child(label.into())
}

impl Render for PoolsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = cx.global::<RulesStore>();
        let build_text = store
            .current_build
            .map(|b| b.0.to_string())
            .unwrap_or_else(|| "<无>".into());
        let rules_dir = store.rules_dir.display().to_string();
        let pool_ids: Vec<PoolId> = store.pools.iter().map(|s| s.pool_id.clone()).collect();
        let pool_kinds: Vec<String> = store
            .pools
            .iter()
            .map(|s| format!("{:?}", s.kind))
            .collect();
        let mut rule_cards: Vec<(CardId, String)> = store
            .rule_set
            .iter()
            .map(|(card, programs)| {
                let statuses = programs
                    .iter()
                    .map(|p| format!("{:?}", p.status))
                    .collect::<Vec<_>>()
                    .join("/");
                (card.clone(), statuses)
            })
            .collect();
        rule_cards.sort_by(|a, b| a.0.cmp(&b.0));
        let errors = store.errors.clone();

        // 控件行
        let controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(t::space_2())
            .child(c::kv("Build", build_text))
            .child(chip(
                "fmt-standard".into(),
                "标准",
                self.format == FormatType::Standard,
                |this, _| this.format = FormatType::Standard,
                cx,
            ))
            .child(chip(
                "fmt-wild".into(),
                "狂野",
                self.format == FormatType::Wild,
                |this, _| this.format = FormatType::Wild,
                cx,
            ))
            .child(chip(
                "live-state".into(),
                if self.use_live_state {
                    "Live 状态: 开"
                } else {
                    "Live 状态: 关"
                },
                self.use_live_state,
                |this, _| this.use_live_state = !this.use_live_state,
                cx,
            ))
            .child(chip(
                "reload-rules".into(),
                "重新加载规则",
                false,
                |this, cx| {
                    cx.set_global(RulesStore::load_default());
                    this.snapshot = None;
                    this.apply_result = None;
                    this.selected_pool = None;
                },
                cx,
            ));

        let class_chips = div()
            .flex()
            .flex_wrap()
            .gap(t::space_1())
            .children(CLASSES.map(|class| {
                let active = self.player_class == class;
                chip(
                    gpui::SharedString::from(format!("class-{class}")),
                    class,
                    active,
                    move |this, _| this.player_class = class.to_string(),
                    cx,
                )
            }));

        // 左栏:池列表 + 规则列表
        let pools_list = div()
            .flex()
            .flex_col()
            .gap(t::space_1())
            .child(c::section_title(format!("池定义 ({})", pool_ids.len())))
            .children(if pool_ids.is_empty() {
                vec![c::empty_state(format!("{rules_dir}/pools/ 下没有池定义")).into_any_element()]
            } else {
                pool_ids
                    .iter()
                    .zip(pool_kinds.iter())
                    .map(|(pool_id, kind)| {
                        let active = self.selected_pool.as_ref() == Some(pool_id);
                        let id_cloned = pool_id.clone();
                        c::list_row()
                            .id(gpui::SharedString::from(format!("pool-{pool_id}")))
                            .cursor_pointer()
                            .when(active, |el| el.bg(t::bg_active()))
                            .hover(|el| el.bg(t::bg_hover()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resolve_pool(id_cloned.clone(), cx);
                                cx.notify();
                            }))
                            .child(div().flex_grow().text_sm().child(pool_id.to_string()))
                            .child(c::badge(kind.clone()))
                            .into_any_element()
                    })
                    .collect()
            });

        let rules_list = div()
            .flex()
            .flex_col()
            .gap(t::space_1())
            .pt(t::space_3())
            .child(c::section_title(format!("卡牌规则 ({})", rule_cards.len())))
            .children(if rule_cards.is_empty() {
                vec![c::empty_state(format!("{rules_dir}/cards/ 下没有规则")).into_any_element()]
            } else {
                rule_cards
                    .iter()
                    .map(|(card_id, statuses)| {
                        let name = crate::cards::row_label(cx, Some(card_id), None);
                        let id_cloned = card_id.clone();
                        c::list_row()
                            .id(gpui::SharedString::from(format!("rule-{card_id}")))
                            .cursor_pointer()
                            .hover(|el| el.bg(t::bg_hover()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.apply_rule(id_cloned.clone(), cx);
                                cx.notify();
                            }))
                            .child(div().flex_grow().text_sm().child(name))
                            .child(c::badge(statuses.clone()))
                            .into_any_element()
                    })
                    .collect()
            });

        let left = div()
            .id("pools-left")
            .flex()
            .flex_col()
            .w_80()
            .h_full()
            .overflow_y_scroll()
            .p(t::space_3())
            .gap(t::space_2())
            .child(pools_list)
            .child(rules_list)
            .children(
                errors
                    .iter()
                    .map(|err| div().text_sm().text_color(t::err()).child(err.clone())),
            );

        // 右栏:快照结果 + 试执行结果
        let mut right = div()
            .id("pools-right")
            .flex()
            .flex_col()
            .flex_grow()
            .h_full()
            .overflow_y_scroll()
            .p(t::space_3())
            .gap(t::space_2());

        right = match &self.snapshot {
            None => right.child(c::empty_state("点击左侧池定义进行求值")),
            Some(Err(err)) => right.child(div().text_sm().text_color(t::err()).child(err.clone())),
            Some(Ok(snapshot)) => {
                let candidates_total = snapshot.candidates.len();
                let shown = snapshot.candidates.iter().take(MAX_CANDIDATE_ROWS);
                right
                    .child(c::section_title(format!("快照: {}", snapshot.pool_id)))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(t::space_2())
                            .child(c::kv("build", snapshot.build_id.0.to_string()))
                            .child(c::kv("exact", snapshot.exact.to_string()))
                            .child(c::kv("候选", candidates_total.to_string()))
                            .child(c::kv(
                                "hash",
                                snapshot.context_hash.chars().take(16).collect::<String>(),
                            )),
                    )
                    .child(c::section_title("过滤 Trace"))
                    .children(snapshot.trace.iter().take(MAX_TRACE_ROWS).map(|entry| {
                        div()
                            .flex()
                            .gap(t::space_2())
                            .text_sm()
                            .child(
                                div()
                                    .w_64()
                                    .text_color(t::text_muted())
                                    .child(entry.step.clone()),
                            )
                            .child(format!("-{} → 剩 {}", entry.removed, entry.remaining))
                    }))
                    .child(c::section_title(format!(
                        "候选(显示前 {} / 共 {})",
                        MAX_CANDIDATE_ROWS.min(candidates_total),
                        candidates_total
                    )))
                    .children(shown.map(|card| {
                        let name = crate::cards::row_label(cx, Some(&card.card_id), card.dbf_id);
                        c::list_row()
                            .child(div().flex_grow().text_sm().child(name))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(t::text_muted())
                                    .child(format!("w={}", card.weight)),
                            )
                    }))
            }
        };

        if let Some((card_id, summary, lines)) = &self.apply_result {
            let name = crate::cards::row_label(cx, Some(card_id), None);
            right = right
                .child(c::section_title(format!("试执行: {name}")))
                .child(div().text_sm().child(summary.clone()))
                .children(lines.iter().take(MAX_TRACE_ROWS).map(|line| {
                    div()
                        .text_sm()
                        .text_color(t::text_muted())
                        .child(line.clone())
                }));
            if lines.len() > MAX_TRACE_ROWS {
                right = right.child(
                    div()
                        .text_sm()
                        .text_color(t::text_faint())
                        .child(format!("……共 {} 行,余下省略", lines.len())),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t::bg_content())
            .text_color(t::text_primary())
            .child(
                div()
                    .p(t::space_3())
                    .flex()
                    .flex_col()
                    .gap(t::space_2())
                    .child(c::page_title("Pool / Rule Inspector"))
                    .child(controls)
                    .child(class_chips)
                    // 当前生效的规则目录:三级查找(环境变量 / .app 包内 / CWD)命中哪一份
                    // 只看列表内容是分不出来的,打包后尤其需要它来判断读的是不是包里那份。
                    .child(c::kv("规则目录", rules_dir.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_grow()
                    .overflow_hidden()
                    .child(left)
                    .child(right),
            )
    }
}
