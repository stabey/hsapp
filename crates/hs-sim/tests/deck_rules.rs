//! 指定牌组(标准任务猎,2026-08-24 用户 deckstring)的规则覆盖测试
//! (M3 验收:牌组 30 张全有规则文件;Verified/Partial 的 IR 真正执行过;
//! 未支持路径显式报告;引用的池全部存在)。
//!
//! rules/ 目录缺失时静默跳过(下游仅拷贝 crate 的场景)。

use hs_effect_ir::{EffectProgram, RuleSet};
use hs_pools::{PoolSnapshot, PoolTraceEntry, WeightedCard};
use hs_sim::{
    apply_program, OutcomeSet, PoolResolver, SimEntity, SimEntityActionState, SimError,
    SimGameState, SimPlayer, SimRequest, TargetBinding,
};
use hs_types::{BuildId, CardId, CardType, EntityId, PlayerId, PoolId, RuleStatus, Zone};
use std::collections::BTreeSet;
use std::path::PathBuf;

const DECK_BUILD: BuildId = BuildId(249896);

/// 牌组 30 张(19 种主卡 + 3 张下水道之王私藏)与期望的规则状态。
const DECK: &[(&str, RuleStatus)] = &[
    ("CORE_BAR_801", RuleStatus::Verified),
    ("CORE_EX1_014", RuleStatus::Verified),
    ("JAIL_877", RuleStatus::Verified),
    ("TIME_053", RuleStatus::Verified),
    ("CORE_DS1_184", RuleStatus::Partial),
    ("CORE_UNG_912", RuleStatus::Partial),
    ("CATA_554", RuleStatus::Partial),
    ("TLC_603", RuleStatus::Partial),
    ("TLC_823", RuleStatus::Partial),
    ("JAIL_EVENT_100", RuleStatus::Partial),
    ("JAIL_878", RuleStatus::Partial),
    ("CORE_EDR_004_2026", RuleStatus::Partial),
    ("CORE_SW_072", RuleStatus::Partial),
    ("JAIL_831", RuleStatus::Partial),
    ("EDR_846", RuleStatus::Partial),
    ("EDR_453", RuleStatus::Partial),
    ("TLC_830", RuleStatus::Unknown),
    ("TLC_825", RuleStatus::Unknown),
    ("TLC_366", RuleStatus::Unknown),
    ("EDR_527", RuleStatus::Unknown),
    ("EDR_489", RuleStatus::Unknown),
    ("TLC_436", RuleStatus::Unknown),
];

fn rules_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules")
}

fn sandbox() -> (SimGameState, TargetBinding) {
    let mut state = SimGameState::default();
    let me = PlayerId(1);
    let foe = PlayerId(2);
    state.friendly_player = Some(me);
    for (player, hero_hp) in [(me, 30), (foe, 30)] {
        state.players.insert(
            player,
            SimPlayer {
                player_id: player,
                hero_health: hero_hp,
                armor: 0,
                mana: 10,
                max_mana: 10,
                overloaded: 0,
                deck_count: 20,
                hand_count: 4,
                fatigue: 0,
            },
        );
    }
    // 友方随从：Effect IR Attack 的唯一 source 绑定。
    state.entities.insert(
        EntityId(40),
        SimEntity {
            entity_id: EntityId(40),
            card_id: Some(CardId::new("TEST_ATTACKER")),
            card_type: Some(CardType::Minion),
            zone: Zone::Play,
            controller: Some(me),
            attack: 3,
            health: 4,
            damage: 0,
            cost: 3,
            action: SimEntityActionState {
                attack_known: true,
                health_known: true,
                ..SimEntityActionState::default()
            },
            simulated: false,
        },
    );
    // 敌方随从:伤害/置生命/摧毁类操作的绑定目标。
    state.entities.insert(
        EntityId(50),
        SimEntity {
            entity_id: EntityId(50),
            card_id: Some(CardId::new("TEST_TARGET")),
            card_type: Some(CardType::Minion),
            zone: Zone::Play,
            controller: Some(foe),
            attack: 4,
            health: 6,
            damage: 0,
            cost: 4,
            action: SimEntityActionState {
                attack_known: true,
                health_known: true,
                ..SimEntityActionState::default()
            },
            simulated: false,
        },
    );
    // 敌方武器:锈烂蝰蛇(CORE_SW_072)的战吼要摧毁 card_type=WEAPON 的实体。
    // M3 时 SimEntity 无 card_type,该 Selector 只能报 InsufficientState;
    // 补上类型后这里必须有真武器,否则测的只是「过滤不到所以没报错」。
    state.entities.insert(
        EntityId(51),
        SimEntity {
            entity_id: EntityId(51),
            card_id: Some(CardId::new("TEST_WEAPON")),
            card_type: Some(CardType::Weapon),
            zone: Zone::Play,
            controller: Some(foe),
            attack: 3,
            health: 2,
            damage: 0,
            cost: 3,
            action: Default::default(),
            simulated: false,
        },
    );
    let binding = TargetBinding {
        source: Some(EntityId(40)),
        target: Some(EntityId(50)),
    };
    (state, binding)
}

/// mock 池:固定三张候选(执行随机操作用;池语义本身由 hs-pools 测试)。
struct FixedPools;

impl PoolResolver for FixedPools {
    fn snapshot(&self, pool_id: &PoolId) -> Result<PoolSnapshot, SimError> {
        let candidates = ["POOL_A", "POOL_B", "POOL_C"]
            .into_iter()
            .map(|id| WeightedCard {
                card_id: CardId::new(id),
                dbf_id: None,
                weight: 1.0,
            })
            .collect::<Vec<_>>();
        Ok(PoolSnapshot {
            pool_id: pool_id.clone(),
            build_id: DECK_BUILD,
            context_hash: "deck-rules-fixed".into(),
            candidates,
            trace: vec![PoolTraceEntry {
                step: "initial".into(),
                removed: 0,
                remaining: 3,
            }],
            exact: true,
        })
    }
}

/// 把 program 的 play 与每个 trigger 的效果序列各作为一次 apply 执行,
/// 返回每段的结果(空段跳过)。
fn execute_all_segments(program: &EffectProgram) -> Vec<OutcomeSet> {
    let mut outcomes = Vec::new();
    let mut segments: Vec<Vec<hs_effect_ir::Operation>> = Vec::new();
    if !program.play.is_empty() {
        segments.push(program.play.clone());
    }
    for trigger in &program.triggers {
        if !trigger.effects.is_empty() {
            segments.push(trigger.effects.clone());
        }
    }
    for effects in segments {
        let mut synthetic = program.clone();
        synthetic.play = effects;
        synthetic.triggers = Vec::new();
        let (state, binding) = sandbox();
        let result = apply_program(
            &state,
            &synthetic,
            &binding,
            &FixedPools,
            &SimRequest::default(),
        )
        .unwrap_or_else(|e| panic!("{} 执行出错: {e}", program.card_id));
        outcomes.push(result.outcome);
    }
    outcomes
}

#[test]
fn deck_cards_all_have_rules_with_expected_status() {
    let dir = rules_dir();
    if !dir.is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    for (card, expected_status) in DECK {
        let card_id = CardId::new(*card);
        let program = set
            .select(DECK_BUILD, &card_id)
            .unwrap_or_else(|| panic!("牌组卡 {card} 缺规则文件"));
        assert_eq!(
            program.status, *expected_status,
            "{card} 的 status 与覆盖测试期望不一致"
        );
    }
}

#[test]
fn verified_and_partial_programs_actually_execute() {
    let dir = rules_dir();
    if !dir.is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    for (card, status) in DECK {
        if matches!(status, RuleStatus::Unknown) {
            continue;
        }
        let card_id = CardId::new(*card);
        let program = set.select(DECK_BUILD, &card_id).expect("有规则");
        let outcomes = execute_all_segments(program);
        // TIME_053 白板:没有可执行段也算通过(无效果即确定性)。
        if outcomes.is_empty() {
            assert_eq!(*card, "TIME_053", "{card} 是 Verified/Partial 却没有效果段");
            continue;
        }
        for outcome in outcomes {
            match outcome {
                OutcomeSet::Deterministic(_) | OutcomeSet::Exact(_) | OutcomeSet::Sampled(_) => {}
                OutcomeSet::Unsupported(reason) => {
                    panic!("{card} 意外 Unsupported: {reason:?}")
                }
                OutcomeSet::Lazy(_) => panic!("{card} 不应产出 Lazy(M3 未实现)"),
            }
        }
    }
}

#[test]
fn unknown_cards_have_no_executable_content() {
    let dir = rules_dir();
    if !dir.is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    for (card, status) in DECK {
        if !matches!(status, RuleStatus::Unknown) {
            continue;
        }
        let program = set.select(DECK_BUILD, &CardId::new(*card)).expect("有规则");
        assert!(
            program.play.is_empty() && program.triggers.is_empty(),
            "{card} 标 Unknown 却带可执行内容,应改 status"
        );
    }
}

#[test]
fn referenced_pools_all_exist() {
    let dir = rules_dir();
    if !dir.is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    let mut defined: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(dir.join("pools")).expect("读取 rules/pools") {
        let path = entry.expect("目录项").path();
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml") | Some("yml")
        ) {
            continue;
        }
        let spec = hs_pools::parse_spec(&std::fs::read_to_string(&path).expect("读池文件"))
            .expect("池文件可解析");
        defined.insert(spec.pool_id.to_string());
    }
    for (card, _) in DECK {
        let program = set.select(DECK_BUILD, &CardId::new(*card)).expect("有规则");
        for pool in referenced_pools(program) {
            assert!(
                defined.contains(&pool),
                "{card} 引用的池 {pool} 在 rules/pools 中不存在"
            );
        }
    }
}

/// 收集 program 里 Discover/RandomChoice 引用的池 ID(递归控制流)。
fn referenced_pools(program: &EffectProgram) -> Vec<String> {
    fn walk(ops: &[hs_effect_ir::Operation], out: &mut Vec<String>) {
        for op in ops {
            match op {
                hs_effect_ir::Operation::Discover { pool, .. }
                | hs_effect_ir::Operation::RandomChoice { pool, .. } => out.push(pool.to_string()),
                hs_effect_ir::Operation::Repeat { effects, .. }
                | hs_effect_ir::Operation::ForEach { effects, .. } => walk(effects, out),
                hs_effect_ir::Operation::Conditional {
                    then, otherwise, ..
                } => {
                    walk(then, out);
                    walk(otherwise, out);
                }
                hs_effect_ir::Operation::ChooseOne { options } => {
                    for option in options {
                        walk(option, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&program.play, &mut out);
    for trigger in &program.triggers {
        walk(&trigger.effects, &mut out);
    }
    out
}

/// A6:锈烂蝰蛇(CORE_SW_072)战吼真正摧毁敌方武器。
///
/// M3 时 `SimEntity` 无 `card_type`,`Selector::Filter` 的 `card_type: WEAPON`
/// 条件只能报 `InsufficientState`(该卡曾在 `expected_unsupported` 白名单里)。
/// 补上类型投影后必须真执行:断言武器进坟、随从不受影响 —— 而不只是
/// 「过滤不到所以没报错」。
#[test]
fn rustrot_viper_destroys_enemy_weapon() {
    let dir = rules_dir();
    if !dir.join("cards").is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    let card_id = CardId::new("CORE_SW_072");
    let program = set.select(DECK_BUILD, &card_id).expect("有规则");

    // 该卡效果挂 battlecry trigger,取其 effects 单独执行(与 execute_all_segments 同法)。
    let effects = program
        .triggers
        .iter()
        .find(|t| !t.effects.is_empty())
        .map(|t| t.effects.clone())
        .expect("锈烂蝰蛇应有 battlecry effects");
    let mut synthetic = program.clone();
    synthetic.play = effects;
    synthetic.triggers = Vec::new();

    let (state, binding) = sandbox();
    assert_eq!(
        state.entity(EntityId(51)).unwrap().zone,
        Zone::Play,
        "前置:沙盘里有一把在场的敌方武器"
    );

    let out = apply_program(
        &state,
        &synthetic,
        &binding,
        &FixedPools,
        &SimRequest::default(),
    )
    .expect("执行不应 Err");

    let OutcomeSet::Deterministic(after) = out.outcome else {
        panic!("摧毁武器应是确定性结果,得到 {:?}", out.outcome);
    };
    assert_eq!(
        after.entity(EntityId(51)).unwrap().zone,
        Zone::Graveyard,
        "敌方武器应被摧毁进坟"
    );
    assert_eq!(
        after.entity(EntityId(50)).unwrap().zone,
        Zone::Play,
        "敌方随从不是 WEAPON,不应被波及"
    );
}

/// A1:穆克拉(CORE_EX1_014)的战吼经 `apply_program` 真正执行。
///
/// 卡面:战吼——使你的对手获得两根香蕉(EX1_014t 置入对手手牌)。
/// M3 时该卡效果全挂在 triggers 上,`apply_program` 只跑 play,故返回
/// Deterministic + 空 trace(真机自查里 9 张这样的卡之一)。
#[test]
fn mukla_battlecry_gives_opponent_two_bananas() {
    let dir = rules_dir();
    if !dir.join("cards").is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    let program = set
        .select(DECK_BUILD, &CardId::new("CORE_EX1_014"))
        .expect("有规则");
    assert!(
        program.play.is_empty(),
        "前置:该卡效果挂在 triggers 而非 play"
    );

    let (state, binding) = sandbox();
    let foe = PlayerId(2);
    let before = state.player(foe).unwrap().hand_count;

    let out = apply_program(
        &state,
        program,
        &binding,
        &FixedPools,
        &SimRequest::default(),
    )
    .expect("执行不应 Err");

    let OutcomeSet::Deterministic(after) = out.outcome else {
        panic!("战吼置牌应是确定性结果,得到 {:?}", out.outcome);
    };
    assert!(!out.trace.entries.is_empty(), "trace 不应再为空");

    let bananas: Vec<_> = after
        .entities
        .values()
        .filter(|e| {
            e.controller == Some(foe)
                && e.zone == Zone::Hand
                && e.card_id.as_ref().map(CardId::as_str) == Some("EX1_014t")
        })
        .collect();
    assert_eq!(bananas.len(), 2, "对手手牌应多出两根香蕉");
    assert!(bananas.iter().all(|e| e.simulated), "应是模拟新建实体");
    assert_eq!(
        after.player(foe).unwrap().hand_count,
        before + 2,
        "手牌计数同步"
    );
}

/// A1:效果挂 triggers 的卡经 `apply_program` 后 trace 不再为空。
///
/// M3 真机自查:25 张建档卡里 16 张 Deterministic + 空 trace,其中 9 张是
/// 效果全在 triggers(另 7 张本就无效果段,空 trace 正确)。本测试盯住这 9 张。
#[test]
fn trigger_only_cards_no_longer_produce_empty_trace() {
    let dir = rules_dir();
    if !dir.join("cards").is_dir() {
        return;
    }
    let set = RuleSet::load_dir(&dir.join("cards")).expect("加载 rules/cards");
    let mut checked = 0;
    for (card_id, _) in set.iter() {
        let Some(program) = set.select(DECK_BUILD, card_id) else {
            continue;
        };
        // 只看「无 play、但有带 effects 的触发」的卡。
        if !program.play.is_empty() {
            continue;
        }
        if !program.triggers.iter().any(|t| !t.effects.is_empty()) {
            continue;
        }
        checked += 1;
        let (state, binding) = sandbox();
        let out = apply_program(
            &state,
            program,
            &binding,
            &FixedPools,
            &SimRequest::default(),
        )
        .unwrap_or_else(|e| panic!("{card_id} 执行出错: {e}"));
        assert!(
            !out.trace.entries.is_empty(),
            "{card_id}: 有触发效果却仍是空 trace"
        );
    }
    assert!(checked >= 9, "应覆盖到 9 张纯触发卡,实际 {checked}");
}
