//! 建议调度会话：唯一 `GameSession` 的去抖、后台计算与新鲜度门禁。
//!
//! 这里只管调度，不拥有规则、卡牌或卡池资源的加载逻辑，也不把结果推给 UI。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, Task, WeakEntity};
use hs_advisor_api::{AdviceIdentity, AdviceResult, SearchConfig, UnavailableReason};
use hs_deckstrings::DeckList;
use hs_observed_state::{state_hash, ObservedGameState};
use hs_planner::{
    prepare_request, Engine, PlannerError, PlannerRequest, PlannerResources, PlannerResult,
};

use crate::advisor_resources::{AdvisorJob, AdvisorResourceCatalog};
use crate::rules_store::RulesStore;
use crate::session::GameSession;
use crate::stores::{active_deck_list, Stores};

const DEBOUNCE: Duration = Duration::from_millis(150);

/// 可以被界面安全观察的状态摘要；`Ready` 不携带原始结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdviceSessionStatus {
    Unavailable(UnavailableReason),
    Debouncing,
    Searching,
    Ready,
    Failed { message: String },
}

/// 全局只注册一个调度实体，后续页面从这里获取句柄。
pub struct AdviceSessionHandle(pub Entity<AdviceSession>);

impl Global for AdviceSessionHandle {}

#[derive(Clone)]
struct AdviceRuntime {
    engine: Arc<dyn Engine>,
    resources: PlannerResources,
}

/// 生产资源目录和测试注入共用同一套调度/新鲜度门禁。
#[derive(Clone)]
enum AdviceBackend {
    Injected(AdviceRuntime),
    Production(Arc<AdvisorResourceCatalog>),
}

enum PreparedAdvice {
    Injected {
        engine: Arc<dyn Engine>,
        request: PlannerRequest,
    },
    Production(AdvisorJob),
}

impl AdviceBackend {
    fn prepare(
        self,
        snapshot: ObservedGameState,
        deck: Option<DeckList>,
        config: SearchConfig,
    ) -> Result<PreparedAdvice, PlannerError> {
        match self {
            Self::Injected(runtime) => {
                let request = prepare_request(snapshot, runtime.resources, config)?;
                Ok(PreparedAdvice::Injected {
                    engine: runtime.engine,
                    request,
                })
            }
            Self::Production(catalog) => catalog
                .prepare_job(snapshot, deck, config)
                .map(PreparedAdvice::Production),
        }
    }
}

impl PreparedAdvice {
    fn identity(&self) -> &AdviceIdentity {
        match self {
            Self::Injected { request, .. } => request.identity(),
            Self::Production(job) => job.identity(),
        }
    }

    fn run(self) -> Result<PlannerResult, PlannerError> {
        match self {
            Self::Injected { engine, request } => engine.plan(request),
            Self::Production(job) => job.run(),
        }
    }
}

struct ReadyAdvice {
    generation: u64,
    result: AdviceResult,
}

/// 不依赖 GPUI 执行器的纯状态核，所有正确性门禁集中在这里。
struct AdviceStateCore {
    generation: u64,
    status: AdviceSessionStatus,
    active_identity: Option<AdviceIdentity>,
    ready: Option<ReadyAdvice>,
}

impl Default for AdviceStateCore {
    fn default() -> Self {
        Self {
            generation: 0,
            status: AdviceSessionStatus::Unavailable(UnavailableReason::MissingBuildData),
            active_identity: None,
            ready: None,
        }
    }
}

impl AdviceStateCore {
    /// 必须在任何同步准备工作前调用，保证旧 Ready 当场不可读。
    fn invalidate(&mut self) -> u64 {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("advice generation exhausted");
        self.active_identity = None;
        self.ready = None;
        self.generation
    }

    fn set_unavailable(&mut self, reason: UnavailableReason) {
        self.status = AdviceSessionStatus::Unavailable(reason);
        self.active_identity = None;
        self.ready = None;
    }

    fn set_failed(&mut self, message: String) {
        self.status = AdviceSessionStatus::Failed { message };
        self.active_identity = None;
        self.ready = None;
    }

    fn begin_debouncing(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.status = AdviceSessionStatus::Debouncing;
        self.active_identity = None;
        true
    }

    fn begin_searching(&mut self, generation: u64, identity: AdviceIdentity) -> bool {
        if generation != self.generation || self.status != AdviceSessionStatus::Debouncing {
            return false;
        }
        self.status = AdviceSessionStatus::Searching;
        self.active_identity = Some(identity);
        true
    }

    fn settle_preparation_error(&mut self, generation: u64, error: PlannerError) -> bool {
        if generation != self.generation || self.status != AdviceSessionStatus::Debouncing {
            return false;
        }
        match error {
            PlannerError::Unavailable(reason) => self.set_unavailable(reason),
            PlannerError::Failed { message } => self.set_failed(message),
        }
        true
    }

    fn completion_is_current(
        &self,
        generation: u64,
        job_identity: &AdviceIdentity,
        current_identity: Option<&AdviceIdentity>,
    ) -> bool {
        generation == self.generation
            && self.status == AdviceSessionStatus::Searching
            && self.active_identity.as_ref() == Some(job_identity)
            && current_identity == Some(job_identity)
    }

    fn publish(
        &mut self,
        generation: u64,
        job_identity: &AdviceIdentity,
        current_identity: Option<&AdviceIdentity>,
        result: AdviceResult,
    ) -> bool {
        if !self.completion_is_current(generation, job_identity, current_identity)
            || result.identity() != job_identity
        {
            return false;
        }
        self.status = AdviceSessionStatus::Ready;
        self.active_identity = None;
        self.ready = Some(ReadyAdvice { generation, result });
        true
    }

    fn settle_error(
        &mut self,
        generation: u64,
        job_identity: &AdviceIdentity,
        current_identity: Option<&AdviceIdentity>,
        error: PlannerError,
    ) -> bool {
        if !self.completion_is_current(generation, job_identity, current_identity) {
            return false;
        }
        match error {
            PlannerError::Unavailable(reason) => self.set_unavailable(reason),
            PlannerError::Failed { message } => self.set_failed(message),
        }
        true
    }

    fn current_for(&self, state: &ObservedGameState) -> Option<&AdviceResult> {
        let current_identity = identity_for_state(state)?;
        let ready = self.ready.as_ref()?;
        (self.status == AdviceSessionStatus::Ready
            && ready.generation == self.generation
            && ready.result.identity() == &current_identity)
            .then_some(&ready.result)
    }
}

/// GPUI 外壳：负责观察、定时和在前后台执行器间传递 owned 值。
pub struct AdviceSession {
    game_session: Entity<GameSession>,
    backend: Option<AdviceBackend>,
    search_config: SearchConfig,
    core: AdviceStateCore,
    _observe: Subscription,
    task: Option<Task<()>>,
}

impl AdviceSession {
    fn new(game_session: Entity<GameSession>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&game_session, |this, game_session, cx| {
            let snapshot = game_session.read(cx).state.clone();
            this.schedule(snapshot, cx);
        });
        Self {
            game_session,
            backend: None,
            search_config: SearchConfig::default(),
            core: AdviceStateCore::default(),
            _observe: observe,
            task: None,
        }
    }

    /// 公开状态不包含 Ready 的原始结果。
    pub fn status(&self) -> &AdviceSessionStatus {
        &self.core.status
    }

    /// 测试/诊断注入入口。注入后立即按当前快照重新调度。
    pub fn set_runtime(
        &mut self,
        engine: Arc<dyn Engine>,
        resources: PlannerResources,
        cx: &mut Context<Self>,
    ) {
        self.backend = Some(AdviceBackend::Injected(AdviceRuntime { engine, resources }));
        let snapshot = self.game_session.read(cx).state.clone();
        self.schedule(snapshot, cx);
    }

    /// 生产资源入口。每个快照会在后台按其精确 Build 准备独立 Beam job。
    pub fn set_resource_catalog(
        &mut self,
        catalog: Arc<AdvisorResourceCatalog>,
        cx: &mut Context<Self>,
    ) {
        self.backend = Some(AdviceBackend::Production(catalog));
        let snapshot = self.game_session.read(cx).state.clone();
        self.schedule(snapshot, cx);
    }

    /// 对外唯一结果读取入口。即使内部是 Ready，也会再核对完整身份。
    pub fn current_for(&self, state: &ObservedGameState) -> Option<&AdviceResult> {
        self.core.current_for(state)
    }

    fn schedule(&mut self, snapshot: ObservedGameState, cx: &mut Context<Self>) {
        // Task 取消只用于节流；即使后台同步 plan 已经开始，下面的纯门禁仍会拒绝它。
        drop(self.task.take());
        let generation = self.core.invalidate();
        debug_assert!(self.current_for(&snapshot).is_none());
        // generation 与 Ready 在同一个实体更新里同步切换。
        cx.notify();

        let Some(backend) = self.backend.clone() else {
            self.core
                .set_unavailable(UnavailableReason::MissingBuildData);
            cx.notify();
            return;
        };

        // 只在 GPUI 线程取一次活动牌组 owned 值；资源绑定与 SQLite 查询均在后台。
        let deck = if matches!(&backend, AdviceBackend::Production(_)) && cx.has_global::<Stores>()
        {
            active_deck_list(cx)
        } else {
            None
        };
        if !self.core.begin_debouncing(generation) {
            return;
        }
        cx.notify();

        let weak: WeakEntity<Self> = cx.entity().downgrade();
        let search_config = self.search_config;
        self.task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(DEBOUNCE).await;

            // 精确 Build 资源准备（包括 SQLite）和同步搜索都不占用 GPUI 线程。
            let prepared = cx
                .background_executor()
                .spawn(async move { backend.prepare(snapshot, deck, search_config) })
                .await;
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    let _ = weak.update(cx, |this, cx| {
                        if this.core.settle_preparation_error(generation, error) {
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let identity = prepared.identity().clone();
            let Ok(true) = weak.update(cx, |this, cx| {
                let started = this.core.begin_searching(generation, identity.clone());
                if started {
                    cx.notify();
                }
                started
            }) else {
                return;
            };

            let planned = cx
                .background_executor()
                .spawn(async move { prepared.run() })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.finish(generation, &identity, planned, cx);
            });
        }));
    }

    fn finish(
        &mut self,
        generation: u64,
        job_identity: &AdviceIdentity,
        planned: Result<hs_planner::PlannerResult, PlannerError>,
        cx: &mut Context<Self>,
    ) {
        let current_identity = identity_for_state(&self.game_session.read(cx).state);
        let changed = match planned {
            Ok(result) if result.identity() == job_identity => self.core.publish(
                generation,
                job_identity,
                current_identity.as_ref(),
                result.into_advice_result(Utc::now()),
            ),
            Ok(_) => self.core.settle_error(
                generation,
                job_identity,
                current_identity.as_ref(),
                PlannerError::Failed {
                    message: "planner returned a mismatched advice identity".into(),
                },
            ),
            Err(error) => {
                self.core
                    .settle_error(generation, job_identity, current_identity.as_ref(), error)
            }
        };
        if changed {
            cx.notify();
        }
    }
}

/// 只投影新鲜度身份；可否启动仍由 planner `prepare_request` 唯一判定。
fn identity_for_state(state: &ObservedGameState) -> Option<AdviceIdentity> {
    Some(AdviceIdentity {
        game_id: state.game_id?,
        state_revision: state.revision,
        state_hash: state_hash(state),
        options_id: state.options.as_ref()?.id,
        build_id: state.build_id?,
    })
}

/// 创建唯一建议会话，接入生产资源目录并注册全局句柄。
pub(crate) fn init(game_session: Entity<GameSession>, cx: &mut App) -> Entity<AdviceSession> {
    let catalog = AdvisorResourceCatalog::from_store(cx.global::<RulesStore>());
    let advice = cx.new(|cx| AdviceSession::new(game_session, cx));
    cx.set_global(AdviceSessionHandle(advice.clone()));
    advice.update(cx, |this, cx| match catalog {
        Ok(catalog) => this.set_resource_catalog(catalog, cx),
        Err(error) => {
            this.core.invalidate();
            match error {
                PlannerError::Unavailable(reason) => this.core.set_unavailable(reason),
                PlannerError::Failed { message } => this.core.set_failed(message),
            }
            cx.notify();
        }
    });
    advice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice_presenter::{LiveAdviceModel, OverlayAdviceModel};
    use hs_advisor_api::{
        AdviceLine, AdvicePayload, ConfidenceBreakdown, LethalAssessment, RuleCoverageReport,
        ScoreBreakdown, SearchCompleteness,
    };
    use hs_pools::PoolSnapshot;
    use hs_sim::{CardLookup, NoRules, PoolResolver, SimCardDefinition, SimError};
    use hs_types::{BuildId, CardId, GameId, PlayerId, PoolId};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn observed(game: u64, revision: u64, options: u64, build: u32) -> ObservedGameState {
        let mut state = ObservedGameState::default();
        state.game_id = Some(GameId(game));
        state.revision = revision;
        state.build_id = Some(BuildId(build));
        state.options = Some(
            serde_json::from_value(json!({
                "id": options,
                "issued_revision": revision,
                "options": []
            }))
            .expect("options fixture"),
        );
        state
    }

    fn advice(identity: &AdviceIdentity) -> AdviceResult {
        serde_json::from_value(json!({
            "identity": {
                "game_id": identity.game_id.0,
                "state_revision": identity.state_revision,
                "state_hash": identity.state_hash,
                "options_id": identity.options_id,
                "build_id": identity.build_id.0
            },
            "payload": {
                "confidence": { "factors": [] },
                "rule_coverage": { "items": [] },
                "opponent_belief_summary": {
                    "opponent_seen_cards": [],
                    "opponent_unknown_hand_count": 0,
                    "known_generation_sources": [],
                    "archetype_probabilities": [],
                    "insufficient_data": true
                },
                "search_completeness": { "total": 1, "reliably_compared": 1 },
                "lines": [{
                    "first_action": {
                        "server_ref": {
                            "options_id": identity.options_id,
                            "option_index": 1,
                            "sub_option_index": null,
                            "target": null,
                            "position": null
                        },
                        "kind": "end_turn",
                        "source": null
                    },
                    "follow_up": [],
                    "score": { "components": [] },
                    "lethal": { "kind": "none" },
                    "confidence": { "factors": [] },
                    "complete": true,
                    "win_point": null,
                    "explanation": "fixture route",
                    "risks": []
                }],
                "explanation": "fixture",
                "warnings": []
            },
            "generated_at": "2026-08-30T00:00:00Z"
        }))
        .expect("advice fixture")
    }

    fn start_search(core: &mut AdviceStateCore, identity: &AdviceIdentity) -> u64 {
        let generation = core.invalidate();
        assert!(core.begin_debouncing(generation));
        assert!(core.begin_searching(generation, identity.clone()));
        generation
    }

    fn publish_ready(core: &mut AdviceStateCore, state: &ObservedGameState) -> u64 {
        let identity = identity_for_state(state).expect("fixture identity");
        let generation = start_search(core, &identity);
        assert!(core.publish(generation, &identity, Some(&identity), advice(&identity)));
        generation
    }

    struct EmptyCards;

    impl CardLookup for EmptyCards {
        fn sim_card(&self, _build: BuildId, _card_id: &CardId) -> Option<SimCardDefinition> {
            None
        }
    }

    struct EmptyPools;

    impl PoolResolver for EmptyPools {
        fn snapshot(&self, _pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
            Err(SimError::NotImplemented("desktop fake engine pool"))
        }
    }

    #[derive(Default)]
    struct FakeEngine {
        calls: AtomicUsize,
    }

    impl Engine for FakeEngine {
        fn plan(
            &self,
            request: hs_planner::PlannerRequest,
        ) -> Result<hs_planner::PlannerResult, PlannerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let first_action = *request
                .legal_actions()
                .actions
                .first()
                .expect("fake request has a legal root");
            let payload = AdvicePayload {
                confidence: ConfidenceBreakdown::default(),
                rule_coverage: RuleCoverageReport::default(),
                opponent_belief_summary: request.belief().summary(),
                search_completeness: SearchCompleteness::new(1, 1).expect("fake completeness"),
                lines: vec![AdviceLine {
                    first_action,
                    follow_up: Vec::new(),
                    score: ScoreBreakdown::default(),
                    chance: Default::default(),
                    lethal: LethalAssessment::None,
                    confidence: ConfidenceBreakdown::default(),
                    complete: true,
                    win_point: None,
                    explanation: "fake engine".into(),
                    risks: Vec::new(),
                }],
                explanation: "fake engine".into(),
                warnings: Vec::new(),
            };
            hs_planner::PlannerResult::for_request(&request, payload)
        }
    }

    fn plannable_state(game: u64, revision: u64, options: u64) -> ObservedGameState {
        let mut state = observed(game, revision, options, 17);
        state.friendly_player = Some(PlayerId(1));
        state.current_player = Some(PlayerId(1));
        state.options = Some(
            serde_json::from_value(json!({
                "id": options,
                "issued_revision": revision,
                "options": [{
                    "index": 1,
                    "option_type": "END_TURN",
                    "entity": null,
                    "entity_raw": "",
                    "error": null,
                    "availability": "Allowed",
                    "raw_error": "NONE",
                    "error_param": "",
                    "targets": [],
                    "target_entries": [],
                    "sub_options": []
                }]
            }))
            .expect("plannable options fixture"),
        );
        state
    }

    fn planner_resources(build_id: BuildId) -> PlannerResources {
        PlannerResources::new(
            build_id,
            Arc::new(NoRules),
            Arc::new(EmptyCards),
            Arc::new(EmptyPools),
        )
    }

    #[test]
    fn notification_synchronously_removes_ready() {
        let state = observed(7, 11, 13, 17);
        let mut core = AdviceStateCore::default();
        publish_ready(&mut core, &state);
        assert!(core.current_for(&state).is_some());

        core.invalidate();

        assert!(core.current_for(&state).is_none());
    }

    #[test]
    fn three_notifications_allow_only_the_last_generation_to_start_or_publish() {
        let state = observed(7, 11, 13, 17);
        let identity = identity_for_state(&state).unwrap();
        let mut core = AdviceStateCore::default();
        let first = core.invalidate();
        assert!(core.begin_debouncing(first));
        let second = core.invalidate();
        assert!(core.begin_debouncing(second));
        let third = core.invalidate();
        assert!(core.begin_debouncing(third));

        assert!(!core.begin_searching(first, identity.clone()));
        assert!(!core.begin_searching(second, identity.clone()));
        assert!(core.begin_searching(third, identity.clone()));
        assert!(!core.publish(first, &identity, Some(&identity), advice(&identity)));
        assert!(!core.publish(second, &identity, Some(&identity), advice(&identity)));
        assert!(core.publish(third, &identity, Some(&identity), advice(&identity)));
    }

    #[test]
    fn stale_background_preparation_error_cannot_replace_the_latest_debounce() {
        let mut core = AdviceStateCore::default();
        let old_generation = core.invalidate();
        assert!(core.begin_debouncing(old_generation));
        let latest_generation = core.invalidate();
        assert!(core.begin_debouncing(latest_generation));

        assert!(!core.settle_preparation_error(
            old_generation,
            PlannerError::Unavailable(UnavailableReason::MissingBuildData)
        ));
        assert_eq!(core.status, AdviceSessionStatus::Debouncing);
        assert_eq!(core.generation, latest_generation);
    }

    #[test]
    fn old_completion_cannot_replace_a_newer_ready_result() {
        let old_state = observed(7, 11, 13, 17);
        let old_identity = identity_for_state(&old_state).unwrap();
        let mut core = AdviceStateCore::default();
        let old_generation = start_search(&mut core, &old_identity);

        let new_state = observed(7, 12, 14, 17);
        let new_identity = identity_for_state(&new_state).unwrap();
        publish_ready(&mut core, &new_state);
        assert!(core.current_for(&new_state).is_some());

        assert!(!core.publish(
            old_generation,
            &old_identity,
            Some(&old_identity),
            advice(&old_identity)
        ));
        assert_eq!(
            core.current_for(&new_state).unwrap().identity(),
            &new_identity
        );
    }

    #[test]
    fn late_revision_cannot_regress_either_display_model() {
        let old_state = observed(7, 11, 13, 17);
        let old_identity = identity_for_state(&old_state).unwrap();
        let mut core = AdviceStateCore::default();
        let old_generation = start_search(&mut core, &old_identity);

        let new_state = observed(7, 12, 14, 17);
        let new_identity = identity_for_state(&new_state).unwrap();
        let new_generation = start_search(&mut core, &new_identity);
        assert!(core.publish(
            new_generation,
            &new_identity,
            Some(&new_identity),
            advice(&new_identity),
        ));

        let current = core.current_for(&new_state);
        let live_before = LiveAdviceModel::from_current(&core.status, &new_state, current);
        let overlay_before = OverlayAdviceModel::from_current(&core.status, &new_state, current);
        assert!(live_before
            .primary
            .as_ref()
            .is_some_and(|line| line.action.contains("Options 14")));
        assert!(overlay_before
            .action
            .as_ref()
            .is_some_and(|action| action.contains("Options 14")));

        assert!(!core.publish(
            old_generation,
            &old_identity,
            Some(&new_identity),
            advice(&old_identity),
        ));
        let current = core.current_for(&new_state);
        assert_eq!(
            LiveAdviceModel::from_current(&core.status, &new_state, current),
            live_before
        );
        assert_eq!(
            OverlayAdviceModel::from_current(&core.status, &new_state, current),
            overlay_before
        );
    }

    #[test]
    fn synchronous_fake_engine_result_still_passes_the_pure_completion_gate() {
        let old_state = plannable_state(7, 11, 13);
        let request = prepare_request(
            old_state.clone(),
            planner_resources(BuildId(17)),
            SearchConfig::default(),
        )
        .expect("prepare fake request");
        let old_identity = request.identity().clone();
        let mut core = AdviceStateCore::default();
        let old_generation = start_search(&mut core, &old_identity);
        let engine = FakeEngine::default();
        let late = engine
            .plan(request)
            .expect("fake engine result")
            .into_advice_result(Utc::now());
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);

        let new_state = plannable_state(7, 12, 14);
        let new_identity = identity_for_state(&new_state).unwrap();
        publish_ready(&mut core, &new_state);

        assert!(!core.publish(old_generation, &old_identity, Some(&new_identity), late));
        assert_eq!(
            core.current_for(&new_state).unwrap().identity(),
            &new_identity
        );
    }

    #[test]
    fn debounce_wakeups_call_fake_engine_only_for_latest_generation() {
        let state = plannable_state(7, 11, 13);
        let mut requests = Vec::new();
        for _ in 0..3 {
            requests.push(
                prepare_request(
                    state.clone(),
                    planner_resources(BuildId(17)),
                    SearchConfig::default(),
                )
                .expect("prepare fake request"),
            );
        }

        let identity = requests[0].identity().clone();
        let mut core = AdviceStateCore::default();
        // Model the scheduling half of three notifications: each notification
        // installs its own debouncing identity immediately after invalidation.
        // Only the newest timer wake-up can cross `begin_searching` and call
        // the synchronous engine.
        let mut scheduled = Vec::new();
        for _ in 0..3 {
            let generation = core.invalidate();
            assert!(core.begin_debouncing(generation));
            scheduled.push(generation);
        }
        assert_eq!(scheduled, [1, 2, 3]);

        let engine = FakeEngine::default();
        for (generation, request) in scheduled.into_iter().zip(requests) {
            if core.begin_searching(generation, identity.clone()) {
                let result = engine.plan(request).expect("latest fake plan");
                assert!(core.publish(
                    generation,
                    &identity,
                    Some(&identity),
                    result.into_advice_result(Utc::now()),
                ));
            }
        }

        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
        assert!(core.current_for(&state).is_some());
    }

    #[test]
    fn current_for_rejects_every_identity_change() {
        let state = observed(7, 11, 13, 17);
        let mut core = AdviceStateCore::default();
        publish_ready(&mut core, &state);

        let mut other_game = state.clone();
        other_game.game_id = Some(GameId(8));
        let mut other_revision = state.clone();
        other_revision.revision += 1;
        let mut other_hash = state.clone();
        other_hash.turn += 1;
        let mut other_options = state.clone();
        other_options.options.as_mut().unwrap().id += 1;
        let mut other_build = state.clone();
        other_build.build_id = Some(BuildId(18));

        for changed in [
            other_game,
            other_revision,
            other_hash,
            other_options,
            other_build,
        ] {
            assert!(core.current_for(&changed).is_none());
        }
    }

    #[test]
    fn another_game_is_rejected_even_when_revision_and_options_collide() {
        let state = observed(7, 11, 13, 17);
        let mut core = AdviceStateCore::default();
        publish_ready(&mut core, &state);

        let collision = observed(8, 11, 13, 17);

        assert!(core.current_for(&collision).is_none());
    }

    #[test]
    fn every_unavailable_reason_flows_through_the_shared_enum() {
        let state = observed(7, 11, 13, 17);
        let identity = identity_for_state(&state).unwrap();
        let reasons = [
            UnavailableReason::MissingGame,
            UnavailableReason::MissingFriendlyPlayer,
            UnavailableReason::NotFriendlyTurn,
            UnavailableReason::NoOptions,
            UnavailableReason::PendingChoice,
            UnavailableReason::BlocksOpen,
            UnavailableReason::MissingBuild,
            UnavailableReason::MissingBuildData,
            UnavailableReason::NoReliableActions,
        ];

        for reason in reasons {
            let mut core = AdviceStateCore::default();
            let generation = start_search(&mut core, &identity);
            assert!(core.settle_error(
                generation,
                &identity,
                Some(&identity),
                PlannerError::Unavailable(reason)
            ));
            assert_eq!(
                core.status,
                AdviceSessionStatus::Unavailable(reason),
                "{reason:?} must remain the shared API value"
            );
        }
    }
}
