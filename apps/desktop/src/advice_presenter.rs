//! Advice 的纯展示投影。
//!
//! 这里不持有 GPUI 实体，也不读取 [`crate::advisor_session::AdviceSession`]
//! 内部结果。调用方必须先用 `current_for(state)` 完成新鲜度校验，再把返回值传进来。

use std::cmp::Reverse;

use hs_advisor_api::{
    ActionKind, AdviceLine, AdviceResult, AdviceRisk, AdviceRiskKind, AdviceWarning,
    AdviceWarningKind, Confidence, ConfidenceBreakdown, ConfidenceReason, LethalAssessment,
    PlannedAction, RuleCoverageReport, ScoreComponent, ScoreFeature, SearchCompleteness,
    UnavailableReason,
};
use hs_observed_state::ObservedGameState;
use hs_types::RuleStatus;

use crate::advisor_session::AdviceSessionStatus;

const MAX_ALTERNATIVES: usize = 3;
const MAX_KEY_SCORE_COMPONENTS: usize = 4;

/// UI 可用于选择颜色/徽标，但不需要重复解释业务状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdviceDisplayState {
    Updating,
    Ready,
    Unavailable,
    Failed,
}

/// Live 与 Overlay 共用的状态文案映射。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdviceStatusText {
    pub state: AdviceDisplayState,
    pub live: &'static str,
    pub overlay: &'static str,
}

/// 将调度状态集中映射为两种密度的文案。
///
/// `has_current_result` 只能来自 `AdviceSession::current_for(state)`。即使会话状态暂时
/// 仍是 `Ready`，没有当前结果时也按更新中处理，绝不显示旧动作。
pub fn advice_status_text(
    status: &AdviceSessionStatus,
    has_current_result: bool,
) -> AdviceStatusText {
    match status {
        AdviceSessionStatus::Debouncing | AdviceSessionStatus::Searching => AdviceStatusText {
            state: AdviceDisplayState::Updating,
            live: "局面变化，建议更新中",
            overlay: "建议更新中",
        },
        AdviceSessionStatus::Ready if has_current_result => AdviceStatusText {
            state: AdviceDisplayState::Ready,
            live: "建议已就绪",
            overlay: "建议就绪",
        },
        AdviceSessionStatus::Ready => AdviceStatusText {
            state: AdviceDisplayState::Updating,
            live: "局面变化，建议更新中",
            overlay: "建议更新中",
        },
        AdviceSessionStatus::Unavailable(reason) => unavailable_status_text(*reason),
        AdviceSessionStatus::Failed { .. } => AdviceStatusText {
            state: AdviceDisplayState::Failed,
            live: "建议计算失败；请查看诊断日志",
            overlay: "建议暂不可用",
        },
    }
}

fn unavailable_status_text(reason: UnavailableReason) -> AdviceStatusText {
    let (live, overlay) = match reason {
        UnavailableReason::MissingGame => ("等待对局开始", "等待对局"),
        UnavailableReason::MissingFriendlyPlayer => ("正在识别我方玩家", "识别我方中"),
        UnavailableReason::NotFriendlyTurn => ("等待我方行动回合", "等待我方回合"),
        UnavailableReason::NoOptions => ("等待服务器动作表", "等待动作"),
        UnavailableReason::PendingChoice => ("请先完成当前选择", "等待选择完成"),
        UnavailableReason::BlocksOpen => ("局面结算中", "结算中"),
        UnavailableReason::MissingBuild => ("本局缺版本信息，无法可靠建议", "缺版本"),
        UnavailableReason::MissingBuildData => ("没有本局版本的规则/卡牌数据", "缺本局数据"),
        UnavailableReason::NoReliableActions => (
            "当前动作证据不足或含未支持机制，无法可靠建议；请自行判断",
            "证据不足，暂无可靠建议",
        ),
    };
    AdviceStatusText {
        state: AdviceDisplayState::Unavailable,
        live,
        overlay,
    }
}

/// Live 中一条完整路线；只有 `action` 是当前服务器确认的动作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveAdviceLineModel {
    pub action: String,
    pub previews: Vec<String>,
    pub lethal: String,
    pub win_point: Option<String>,
    pub score_total: String,
    pub score_components: Vec<String>,
    pub risks: Vec<String>,
    pub confidence: String,
    pub explanation: String,
}

impl LiveAdviceLineModel {
    fn from_line(line: &AdviceLine, state: &ObservedGameState) -> Self {
        Self {
            action: format_current_action(line, state),
            previews: line
                .follow_up
                .iter()
                .map(|action| format_preview_action(action, state))
                .collect(),
            lethal: format_lethal(line.lethal),
            win_point: line.win_point.clone(),
            score_total: format!("路线得分 {}", format_milli(line.score.total_milli())),
            score_components: key_score_components(line),
            risks: line.risks.iter().map(format_risk).collect(),
            confidence: format_confidence("路线置信度", &line.confidence),
            explanation: line.explanation.clone(),
        }
    }
}

/// Live 所需的完整建议展示数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveAdviceModel {
    pub state: AdviceDisplayState,
    pub status: String,
    pub build: String,
    pub revision: String,
    pub primary: Option<LiveAdviceLineModel>,
    pub alternatives: Vec<LiveAdviceLineModel>,
    pub confidence: Option<String>,
    pub coverage: Option<String>,
    pub search_completeness: Option<String>,
    pub warnings: Vec<String>,
}

impl LiveAdviceModel {
    /// 只在 `status == Ready` 且 `current` 来自当前状态时投影动作数据。
    pub fn from_current(
        status: &AdviceSessionStatus,
        state: &ObservedGameState,
        current: Option<&AdviceResult>,
    ) -> Self {
        let status_text = advice_status_text(status, current.is_some());
        let current = matches!(status_text.state, AdviceDisplayState::Ready)
            .then_some(current)
            .flatten();

        let (primary, alternatives, confidence, coverage, search_completeness, warnings) = current
            .map_or_else(
                || (None, Vec::new(), None, None, None, Vec::new()),
                |result| {
                    let payload = result.payload();
                    (
                        payload
                            .lines
                            .first()
                            .map(|line| LiveAdviceLineModel::from_line(line, state)),
                        payload
                            .lines
                            .iter()
                            .skip(1)
                            .take(MAX_ALTERNATIVES)
                            .map(|line| LiveAdviceLineModel::from_line(line, state))
                            .collect(),
                        Some(format_confidence("建议置信度", &payload.confidence)),
                        Some(format_coverage(&payload.rule_coverage)),
                        Some(format_search_completeness(payload.search_completeness)),
                        payload.warnings.iter().map(format_warning).collect(),
                    )
                },
            );

        Self {
            state: status_text.state,
            status: status_text.live.into(),
            build: state
                .build_id
                .map_or_else(|| "Build 未知".into(), |build| format!("Build {}", build.0)),
            revision: format!("Revision {}", state.revision),
            primary,
            alternatives,
            confidence,
            coverage,
            search_completeness,
            warnings,
        }
    }
}

/// Overlay 的有意裁剪投影：不携带备选路线、长解释、覆盖报告或预演步骤。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayAdviceModel {
    pub state: AdviceDisplayState,
    pub status: String,
    pub action: Option<String>,
    pub confidence: Option<String>,
    pub risk: Option<String>,
}

impl OverlayAdviceModel {
    /// `current` 必须是调用方对同一个 `state` 执行 `current_for` 的结果。
    pub fn from_current(
        status: &AdviceSessionStatus,
        state: &ObservedGameState,
        current: Option<&AdviceResult>,
    ) -> Self {
        let status_text = advice_status_text(status, current.is_some());
        let current = matches!(status_text.state, AdviceDisplayState::Ready)
            .then_some(current)
            .flatten();
        let primary = current.and_then(|result| result.payload().lines.first());

        Self {
            state: status_text.state,
            status: status_text.overlay.into(),
            action: primary.map(|line| format_current_action(line, state)),
            confidence: current
                .map(|result| format_confidence("建议置信度", &result.payload().confidence)),
            risk: primary.and_then(highest_priority_risk).map(format_risk),
        }
    }
}

fn format_current_action(line: &AdviceLine, state: &ObservedGameState) -> String {
    let action = &line.first_action;
    let server = action.server_ref;
    let mut parts = vec![
        "当前可执行".to_string(),
        action_kind_text(action.kind).to_string(),
    ];
    if let Some(source) = action.source {
        parts.push(format_entity("来源", source, state));
    }
    if let Some(target) = server.target {
        parts.push(format_entity("目标", target, state));
    }
    parts.push(format!("Options {}", server.options_id));
    parts.push(format!("操作 {}", server.option_index));
    if let Some(sub_option) = server.sub_option_index {
        parts.push(format!("子项 {sub_option}"));
    }
    if let Some(position) = server.position {
        parts.push(format!("位置 {position}"));
    }
    parts.join(" · ")
}

fn format_preview_action(action: &PlannedAction, state: &ObservedGameState) -> String {
    let mut parts = vec![
        "沙盘预演".to_string(),
        action_kind_text(action.kind).to_string(),
    ];
    if let Some(source) = action.source {
        parts.push(format_entity("来源", source, state));
    }
    if let Some(target) = action.target {
        parts.push(format_entity("目标", target, state));
    }
    if let Some(sub_option) = action.sub_option_index {
        parts.push(format!("子项 {sub_option}"));
    }
    if let Some(position) = action.position {
        parts.push(format!("位置 {position}"));
    }
    parts.join(" · ")
}

fn format_entity(role: &str, entity_id: hs_types::EntityId, state: &ObservedGameState) -> String {
    state
        .entity(entity_id)
        .and_then(|entity| entity.card_id.as_ref())
        .map_or_else(
            || format!("{role}实体 #{}", entity_id.0),
            |card_id| format!("{role} {card_id} (#{})", entity_id.0),
        )
}

fn action_kind_text(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::PlayCard => "出牌",
        ActionKind::Attack => "攻击",
        ActionKind::UseHeroPower => "使用英雄技能",
        ActionKind::ActivateLocation => "使用地点",
        ActionKind::EndTurn => "结束回合",
        ActionKind::Unknown => "未知动作",
    }
}

fn format_lethal(lethal: LethalAssessment) -> String {
    match lethal {
        LethalAssessment::None => "无一回合斩杀".into(),
        LethalAssessment::Guaranteed => "确定一回合斩杀".into(),
        LethalAssessment::Probabilistic { probability } => {
            format!("概率一回合斩杀 {}", format_percentage(probability))
        }
        LethalAssessment::Estimated {
            probability,
            sample_count,
        } => format!(
            "估计一回合斩杀 {}（{sample_count} 个样本）",
            format_percentage(probability)
        ),
        LethalAssessment::Unknown => "一回合斩杀未知".into(),
    }
}

fn format_confidence(label: &str, breakdown: &ConfidenceBreakdown) -> String {
    let confidence = breakdown.confidence();
    let mut text = format!("{label} {}", format_percentage(confidence));
    if confidence < Confidence::ONE {
        if let Some(factor) = breakdown
            .factors()
            .iter()
            .find(|factor| factor.confidence == confidence)
        {
            text.push_str(" · 主要限制：");
            text.push_str(confidence_reason_text(&factor.reason));
        }
    }
    text
}

fn confidence_reason_text(reason: &ConfidenceReason) -> &'static str {
    match reason {
        ConfidenceReason::RuleVerified => "规则已验证",
        ConfidenceReason::RuleInferred => "规则为推断",
        ConfidenceReason::RulePartial => "规则仅部分覆盖",
        ConfidenceReason::RuleStale => "规则需要复核",
        ConfidenceReason::RuleUnknown => "规则未知",
        ConfidenceReason::RuleMissing => "规则缺失",
        ConfidenceReason::RuntimeUnknown => "运行时出现未知语义",
        ConfidenceReason::ServerLegalAction => "当前动作由服务器确认",
        ConfidenceReason::SimulatedLegalAction => "后续动作来自沙盘预演",
        ConfidenceReason::DeterministicSimulation => "确定性模拟证据",
        ConfidenceReason::ExactSimulation => "精确分支模拟证据",
        ConfidenceReason::SampledSimulation => "抽样模拟",
        ConfidenceReason::ProbabilityMassIncomplete => "概率质量不完整",
        ConfidenceReason::SearchIncomplete => "搜索未完整比较",
        ConfidenceReason::BudgetTruncated => "搜索预算截断",
    }
}

fn format_coverage(coverage: &RuleCoverageReport) -> String {
    let total = coverage.total_count();
    if total == 0 {
        return "规则覆盖 · 本路线未涉及卡牌规则".into();
    }

    format!(
        "规则覆盖 {} · 已覆盖 {}/{} · Verified {} · Inferred {} · Partial {} · Stale {} · Unknown {} · Missing {} · Runtime Unknown {}",
        format_percentage(coverage.coverage_ratio()),
        coverage.covered_count(),
        total,
        coverage.verified_count(),
        coverage.status_count(RuleStatus::Inferred),
        coverage.status_count(RuleStatus::Partial),
        coverage.status_count(RuleStatus::Stale),
        coverage.status_count(RuleStatus::Unknown),
        coverage.missing_count(),
        coverage.runtime_unknown_count(),
    )
}

fn format_search_completeness(search: SearchCompleteness) -> String {
    format!(
        "搜索完整度 {} · 可靠比较 {}/{}",
        format_percentage(search.ratio()),
        search.reliably_compared(),
        search.total()
    )
}

fn key_score_components(line: &AdviceLine) -> Vec<String> {
    let mut components: Vec<_> = line
        .score
        .components()
        .iter()
        .copied()
        .filter(|component| component.weighted_milli() != 0)
        .collect();
    components.sort_by_key(|component| {
        (
            Reverse(component.weighted_milli().unsigned_abs()),
            component.feature.stable_index(),
        )
    });
    components
        .into_iter()
        .take(MAX_KEY_SCORE_COMPONENTS)
        .map(format_score_component)
        .collect()
}

fn format_score_component(component: ScoreComponent) -> String {
    format!(
        "{} {}",
        score_feature_text(component.feature),
        format_milli(i128::from(component.weighted_milli()))
    )
}

fn score_feature_text(feature: ScoreFeature) -> &'static str {
    match feature {
        ScoreFeature::FriendlyEffectiveHealth => "我方有效生命",
        ScoreFeature::OpponentEffectiveHealth => "对方有效生命",
        ScoreFeature::FriendlyBoardAttack => "我方场攻",
        ScoreFeature::OpponentBoardAttack => "对方场攻",
        ScoreFeature::FriendlyBoardHealth => "我方场面生命",
        ScoreFeature::OpponentBoardHealth => "对方场面生命",
        ScoreFeature::FriendlyBoardValue => "我方场面价值",
        ScoreFeature::OpponentBoardValue => "对方场面价值",
        ScoreFeature::FriendlyHandSize => "我方手牌",
        ScoreFeature::OpponentHandSize => "对方手牌",
        ScoreFeature::ResourceAdvantage => "资源优势",
        ScoreFeature::ManaEfficiency => "法力效率",
        ScoreFeature::PlaybookPreference => "策略偏好",
        ScoreFeature::RiskPenalty => "风险惩罚",
    }
}

fn highest_priority_risk(line: &AdviceLine) -> Option<&AdviceRisk> {
    line.risks
        .iter()
        .enumerate()
        .min_by_key(|(index, risk)| (risk_priority(risk.kind), *index))
        .map(|(_, risk)| risk)
}

fn risk_priority(kind: AdviceRiskKind) -> u8 {
    match kind {
        AdviceRiskKind::RuntimeUnknown => 0,
        AdviceRiskKind::RuleCoverage => 1,
        AdviceRiskKind::SearchTruncated => 2,
        AdviceRiskKind::SearchIncomplete => 3,
        AdviceRiskKind::HiddenInformation => 4,
        AdviceRiskKind::Randomness => 5,
        AdviceRiskKind::DerivedLegality => 6,
        AdviceRiskKind::Other => 7,
    }
}

fn format_risk(risk: &AdviceRisk) -> String {
    let label = match risk.kind {
        AdviceRiskKind::Randomness => "随机性",
        AdviceRiskKind::RuleCoverage => "规则覆盖",
        AdviceRiskKind::RuntimeUnknown => "运行时未知",
        AdviceRiskKind::DerivedLegality => "预演合法性",
        AdviceRiskKind::HiddenInformation => "隐藏信息",
        AdviceRiskKind::SearchIncomplete => "搜索不完整",
        AdviceRiskKind::SearchTruncated => "搜索已截断",
        AdviceRiskKind::Other => "其它风险",
    };
    if risk.detail.trim().is_empty() {
        label.into()
    } else {
        format!("{label}：{}", risk.detail)
    }
}

fn format_warning(warning: &AdviceWarning) -> String {
    let label = match warning.kind {
        AdviceWarningKind::NoReliableActions => "无可靠动作",
        AdviceWarningKind::MissingRule => "规则缺失",
        AdviceWarningKind::RuntimeUnknown => "运行时未知",
        AdviceWarningKind::InsufficientBeliefData => "隐藏信息不足",
        AdviceWarningKind::SearchIncomplete => "搜索不完整",
        AdviceWarningKind::BudgetExhausted => "搜索预算耗尽",
        AdviceWarningKind::Other => "其它提示",
    };
    if warning.message.trim().is_empty() {
        label.into()
    } else {
        format!("{label}：{}", warning.message)
    }
}

fn format_percentage(confidence: Confidence) -> String {
    format!("{:.0}%", confidence.value() * 100.0)
}

fn format_milli(value: i128) -> String {
    let sign = match value.cmp(&0) {
        std::cmp::Ordering::Less => "-",
        std::cmp::Ordering::Equal => "",
        std::cmp::Ordering::Greater => "+",
    };
    let magnitude = value.unsigned_abs();
    format!("{sign}{}.{:03}", magnitude / 1_000, magnitude % 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hs_advisor_api::AdviceResult;
    use hs_observed_state::EntityState;
    use hs_types::{BuildId, CardId, EntityId, Zone};
    use serde_json::{json, Value};

    fn state() -> ObservedGameState {
        let mut state = ObservedGameState::default();
        state.revision = 42;
        state.build_id = Some(BuildId(17));
        state.entities.insert(
            EntityId(42),
            EntityState::new(EntityId(42), Some(CardId::new("JAIL_877")), Zone::Hand),
        );
        state.entities.insert(
            EntityId(99),
            EntityState::new(EntityId(99), Some(CardId::new("TARGET_001")), Zone::Play),
        );
        state
    }

    fn risk(kind: &str, code: &str, detail: &str) -> Value {
        json!({ "kind": kind, "code": code, "detail": detail })
    }

    fn line(index: u32, lethal: Value, risks: Vec<Value>) -> Value {
        json!({
            "first_action": {
                "server_ref": {
                    "options_id": 71,
                    "option_index": index,
                    "sub_option_index": 2,
                    "target": 99,
                    "position": 3
                },
                "kind": "play_card",
                "source": 42
            },
            "follow_up": [{
                "kind": "attack",
                "source": 42,
                "target": 7,
                "sub_option_index": null,
                "position": null,
                "origin": "simulated"
            }],
            "score": { "components": [
                { "feature": "friendly_effective_health", "raw": 10, "weight_milli": 100 },
                { "feature": "opponent_effective_health", "raw": 10, "weight_milli": -300 },
                { "feature": "mana_efficiency", "raw": 1, "weight_milli": 500 },
                { "feature": "playbook_preference", "raw": 2, "weight_milli": 100 },
                { "feature": "risk_penalty", "raw": 1, "weight_milli": -100 }
            ]},
            "lethal": lethal,
            "confidence": { "factors": [{
                "reason": "sampled_simulation",
                "confidence": 0.65
            }]},
            "complete": false,
            "win_point": "建立下回合斩杀",
            "explanation": format!("路线 {index}"),
            "risks": risks
        })
    }

    fn result() -> AdviceResult {
        let risks = vec![
            risk("randomness", "sampled", "来自抽样分支"),
            risk("runtime_unknown", "unknown", "亡语语义未知"),
            risk("search_truncated", "budget", "达到搜索预算"),
        ];
        let lines = vec![
            line(
                1,
                json!({
                    "kind": "estimated",
                    "probability": 0.4,
                    "sample_count": 64
                }),
                risks,
            ),
            line(2, json!({ "kind": "unknown" }), Vec::new()),
            line(3, json!({ "kind": "none" }), Vec::new()),
            line(4, json!({ "kind": "guaranteed" }), Vec::new()),
            line(5, json!({ "kind": "none" }), Vec::new()),
        ];

        serde_json::from_value(json!({
            "identity": {
                "game_id": 3,
                "state_revision": 42,
                "state_hash": "fixture",
                "options_id": 71,
                "build_id": 17
            },
            "payload": {
                "confidence": { "factors": [{
                    "reason": "search_incomplete",
                    "confidence": 0.72
                }]},
                "rule_coverage": { "items": [
                    { "card_id": "A", "status": "verified", "confidence": 1.0, "runtime_unknown": false },
                    { "card_id": "B", "status": "partial", "confidence": 0.7, "runtime_unknown": false },
                    { "card_id": "C", "status": "unknown", "confidence": 0.2, "runtime_unknown": true },
                    { "card_id": "D", "status": null, "confidence": 0.0, "runtime_unknown": false }
                ]},
                "opponent_belief_summary": {
                    "opponent_seen_cards": [],
                    "opponent_unknown_hand_count": 0,
                    "known_generation_sources": [],
                    "archetype_probabilities": [],
                    "insufficient_data": true
                },
                "search_completeness": {
                    "total": 5,
                    "reliably_compared": 4,
                    "evidence_cap": 0.8
                },
                "lines": lines,
                "explanation": "完整建议说明不会进入 Overlay",
                "warnings": [{
                    "kind": "search_incomplete",
                    "code": "root_not_compared",
                    "message": "一个根动作未可靠比较"
                }]
            },
            "generated_at": "2026-08-30T00:00:00Z"
        }))
        .expect("advice fixture")
    }

    #[test]
    fn every_unavailable_reason_has_shared_live_and_overlay_copy() {
        let cases = [
            (UnavailableReason::MissingGame, "等待对局开始", "等待对局"),
            (
                UnavailableReason::MissingFriendlyPlayer,
                "正在识别我方玩家",
                "识别我方中",
            ),
            (
                UnavailableReason::NotFriendlyTurn,
                "等待我方行动回合",
                "等待我方回合",
            ),
            (UnavailableReason::NoOptions, "等待服务器动作表", "等待动作"),
            (
                UnavailableReason::PendingChoice,
                "请先完成当前选择",
                "等待选择完成",
            ),
            (UnavailableReason::BlocksOpen, "局面结算中", "结算中"),
            (
                UnavailableReason::MissingBuild,
                "本局缺版本信息，无法可靠建议",
                "缺版本",
            ),
            (
                UnavailableReason::MissingBuildData,
                "没有本局版本的规则/卡牌数据",
                "缺本局数据",
            ),
            (
                UnavailableReason::NoReliableActions,
                "当前动作证据不足或含未支持机制，无法可靠建议；请自行判断",
                "证据不足，暂无可靠建议",
            ),
        ];

        for (reason, live, overlay) in cases {
            let copy = advice_status_text(&AdviceSessionStatus::Unavailable(reason), false);
            assert_eq!(copy.state, AdviceDisplayState::Unavailable);
            assert_eq!(copy.live, live);
            assert_eq!(copy.overlay, overlay);
        }
    }

    #[test]
    fn no_reliable_actions_does_not_promise_unavailable_evidence_details() {
        let status = AdviceSessionStatus::Unavailable(UnavailableReason::NoReliableActions);
        let live = LiveAdviceModel::from_current(&status, &state(), None);
        let overlay = OverlayAdviceModel::from_current(&status, &state(), None);

        assert!(live.status.contains("证据不足或含未支持机制"));
        assert!(live.status.contains("请自行判断"));
        assert!(!live.status.contains("查看规则覆盖"));
        assert!(live.coverage.is_none());
        assert!(live.primary.is_none());
        assert_eq!(overlay.status, "证据不足，暂无可靠建议");
        assert!(overlay.action.is_none());
        assert!(overlay.risk.is_none());
    }

    #[test]
    fn updating_and_failed_states_never_expose_current_actions() {
        let result = result();
        for status in [
            AdviceSessionStatus::Debouncing,
            AdviceSessionStatus::Searching,
            AdviceSessionStatus::Failed {
                message: "/Users/private/secret.db: sqlite failed".into(),
            },
        ] {
            let live = LiveAdviceModel::from_current(&status, &state(), Some(&result));
            let overlay = OverlayAdviceModel::from_current(&status, &state(), Some(&result));
            assert!(live.primary.is_none());
            assert!(live.alternatives.is_empty());
            assert!(overlay.action.is_none());
            assert!(!live.status.contains("/Users/private"));
            assert!(!overlay.status.contains("/Users/private"));
        }
    }

    #[test]
    fn ready_live_model_keeps_full_semantics_and_bounds_alternatives() {
        let result = result();
        let model =
            LiveAdviceModel::from_current(&AdviceSessionStatus::Ready, &state(), Some(&result));

        assert_eq!(model.state, AdviceDisplayState::Ready);
        assert_eq!(model.build, "Build 17");
        assert_eq!(model.revision, "Revision 42");
        assert_eq!(model.alternatives.len(), 3);
        assert_eq!(
            model.confidence.as_deref(),
            Some("建议置信度 72% · 主要限制：搜索未完整比较")
        );
        assert!(model.coverage.as_deref().unwrap().contains("规则覆盖 50%"));
        assert_eq!(
            model.search_completeness.as_deref(),
            Some("搜索完整度 80% · 可靠比较 4/5")
        );
        assert_eq!(model.warnings, ["搜索不完整：一个根动作未可靠比较"]);

        let primary = model.primary.expect("primary line");
        assert!(primary.action.contains("当前可执行 · 出牌"));
        assert!(primary.action.contains("来源 JAIL_877 (#42)"));
        assert!(primary.action.contains("目标 TARGET_001 (#99)"));
        assert!(primary.action.contains("Options 71 · 操作 1"));
        assert!(primary.previews[0].starts_with("沙盘预演 · 攻击"));
        assert_eq!(primary.lethal, "估计一回合斩杀 40%（64 个样本）");
        assert_eq!(primary.win_point.as_deref(), Some("建立下回合斩杀"));
        assert_eq!(primary.score_total, "路线得分 -1.400");
        assert_eq!(primary.score_components.len(), MAX_KEY_SCORE_COMPONENTS);
        assert!(primary.score_components[0].starts_with("对方有效生命 -3.000"));
        assert_eq!(primary.confidence, "路线置信度 65% · 主要限制：抽样模拟");
        assert_eq!(primary.risks.len(), 3);
        assert_eq!(model.alternatives[0].lethal, "一回合斩杀未知");
    }

    #[test]
    fn overlay_is_trimmed_and_chooses_the_highest_priority_risk() {
        let result = result();
        let live =
            LiveAdviceModel::from_current(&AdviceSessionStatus::Ready, &state(), Some(&result));
        let overlay =
            OverlayAdviceModel::from_current(&AdviceSessionStatus::Ready, &state(), Some(&result));

        assert_eq!(overlay.state, AdviceDisplayState::Ready);
        assert_eq!(
            overlay.action.as_ref(),
            live.primary.as_ref().map(|line| &line.action)
        );
        assert_eq!(overlay.confidence, live.confidence);
        assert_eq!(overlay.risk.as_deref(), Some("运行时未知：亡语语义未知"));
    }

    #[test]
    fn ready_without_a_current_result_is_treated_as_updating() {
        let live = LiveAdviceModel::from_current(&AdviceSessionStatus::Ready, &state(), None);
        let overlay = OverlayAdviceModel::from_current(&AdviceSessionStatus::Ready, &state(), None);

        assert_eq!(live.state, AdviceDisplayState::Updating);
        assert_eq!(live.status, "局面变化，建议更新中");
        assert!(live.primary.is_none());
        assert_eq!(overlay.status, "建议更新中");
        assert!(overlay.action.is_none());
    }
}
