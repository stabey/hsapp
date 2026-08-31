//! 死亡结算与亡语级联(任务 08-28-sim-triggers-deaths,验收 A2/A3/A4)。
//!
//! 顺序语义的证据与取舍见任务 `research/death-ordering-evidence.md`:
//! D1(判死时机)已由真实日志验证;D2 改为「多亡语时显式不支持」;
//! D3 也已有 6/6 真实样本验证「先离场再结算亡语」。

mod common;

use common::{apply_with_rules, board, deathrattle_card, hero, minion, player, MapRules};
use hs_effect_ir::{Operation, PlayerRef, Selector};
use hs_sim::{OutcomeSet, SimGameState, TargetBinding, UnsupportedReason};
use hs_types::{BuildId, CardId, EntityId, PlayerId, RuleStatus, Zone};

fn state_with(minions: Vec<(i32, &str, i32, i32, i32)>) -> SimGameState {
    let mut entities = vec![hero(4, "HERO_01", 1, 30), hero(5, "HERO_02", 2, 30)];
    for (id, card, ctrl, atk, hp) in minions {
        entities.push(minion(id, card, ctrl, atk, hp));
    }
    board(
        entities,
        vec![player(1, 30, 0, 3, 5, 5, 0), player(2, 30, 0, 3, 5, 5, 0)],
    )
}

fn damage(target: Selector, amount: i32) -> Operation {
    Operation::Damage { target, amount }
}

fn expect_det(outcome: &OutcomeSet) -> &SimGameState {
    match outcome {
        OutcomeSet::Deterministic(state) => state,
        other => panic!("期望 Deterministic,得到 {other:?}"),
    }
}

/// A2:致死伤害 → 进坟 **且** 亡语被执行。
#[test]
fn lethal_damage_moves_to_graveyard_and_fires_deathrattle() {
    // 给死者一个可观测的亡语:对敌方英雄造成 3 点。
    let rules = MapRules::new(vec![deathrattle_card(
        "DR_MINION",
        vec![damage(Selector::EnemyHero, 3)],
    )]);
    let given = state_with(vec![(20, "DR_MINION", 1, 2, 4)]);
    let foe_hp_before = given.player(PlayerId(2)).unwrap().hero_health;

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::Target, 4)],
        TargetBinding {
            source: None,
            target: Some(EntityId(20)),
        },
        &rules,
    );
    let then = expect_det(&out.outcome);

    assert_eq!(
        then.entity(EntityId(20)).unwrap().zone,
        Zone::Graveyard,
        "致死随从应进坟"
    );
    assert_eq!(
        then.player(PlayerId(2)).unwrap().hero_health,
        foe_hp_before - 3,
        "亡语应真正执行(敌方英雄 -3)"
    );

    let ops: Vec<&str> = out.trace.entries.iter().map(|e| e.op.as_str()).collect();
    assert!(ops.contains(&"death"), "trace 应含死亡段: {ops:?}");
    assert!(ops.contains(&"deathrattle"), "trace 应含亡语段: {ops:?}");
    assert!(
        out.unknown_rule_cards.is_empty(),
        "规则查得到时不应记覆盖缺口"
    );
}

/// D3:亡语结算时死亡实体已离场(先移区,再结算)。
///
/// 用「亡语点数己方随从」验证:若死者仍在场,它会被自己的亡语选中。
#[test]
fn deathrattle_sees_itself_already_removed() {
    let rules = MapRules::new(vec![deathrattle_card(
        "DR_AOE",
        vec![damage(Selector::FriendlyMinions, 9)],
    )]);
    // 20 死亡触发亡语,轰己方随从;21 会被轰死,20 自己已离场不该再吃伤害。
    let given = state_with(vec![(20, "DR_AOE", 1, 2, 4), (21, "OTHER", 1, 1, 5)]);

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::Target, 4)],
        TargetBinding {
            source: None,
            target: Some(EntityId(20)),
        },
        &rules,
    );
    let then = expect_det(&out.outcome);

    let dead = then.entity(EntityId(20)).unwrap();
    assert_eq!(dead.zone, Zone::Graveyard);
    assert_eq!(
        dead.damage, 4,
        "死者已离场,不应再吃到自己亡语的 AOE(否则会是 13)"
    );
    assert_eq!(
        then.entity(EntityId(21)).unwrap().zone,
        Zone::Graveyard,
        "同侧随从被亡语 AOE 打死"
    );
}

/// A3:连锁死亡 —— 亡语造成范围伤害再致死另一随从,第二层同样被结算。
#[test]
fn deathrattle_chain_resolves_second_layer() {
    let rules = MapRules::new(vec![
        // 第一层:死亡时 AOE 9 点,打死 21。
        deathrattle_card("DR_AOE", vec![damage(Selector::FriendlyMinions, 9)]),
        // 第二层:21 的亡语打敌方英雄 5 点 —— 只有连锁真的发生才会掉血。
        deathrattle_card("DR_SECOND", vec![damage(Selector::EnemyHero, 5)]),
    ]);
    let given = state_with(vec![(20, "DR_AOE", 1, 2, 4), (21, "DR_SECOND", 1, 1, 5)]);
    let foe_hp_before = given.player(PlayerId(2)).unwrap().hero_health;

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::Target, 4)],
        TargetBinding {
            source: None,
            target: Some(EntityId(20)),
        },
        &rules,
    );
    let then = expect_det(&out.outcome);

    assert_eq!(then.entity(EntityId(20)).unwrap().zone, Zone::Graveyard);
    assert_eq!(
        then.entity(EntityId(21)).unwrap().zone,
        Zone::Graveyard,
        "第二层死亡"
    );
    assert_eq!(
        then.player(PlayerId(2)).unwrap().hero_health,
        foe_hp_before - 5,
        "第二层亡语也被结算"
    );

    // trace 应体现两层。
    let depths: Vec<&str> = out
        .trace
        .entries
        .iter()
        .filter(|e| e.op == "death")
        .map(|e| e.detail.as_str())
        .collect();
    assert_eq!(depths.len(), 2, "两次死亡各一条 trace: {depths:?}");
    assert!(depths[0].contains("depth=0"), "{depths:?}");
    assert!(depths[1].contains("depth=1"), "第二层深度递增: {depths:?}");
}

/// §3.0:多个已知亡语同时死亡 → 显式不支持(真实入场顺序不可知)。
#[test]
fn multiple_simultaneous_deathrattles_are_unsupported() {
    let rules = MapRules::new(vec![
        deathrattle_card("DR_A", vec![damage(Selector::EnemyHero, 1)]),
        deathrattle_card("DR_B", vec![damage(Selector::EnemyHero, 2)]),
    ]);
    let given = state_with(vec![(20, "DR_A", 1, 1, 3), (21, "DR_B", 1, 1, 3)]);

    // AOE 同时打死两个都带亡语的随从。
    let out = apply_with_rules(
        &given,
        vec![damage(Selector::FriendlyMinions, 3)],
        TargetBinding::default(),
        &rules,
    );

    match &out.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
            assert_eq!(what, "multi-deathrattle ordering");
        }
        other => panic!("期望多亡语顺序不支持,得到 {other:?}"),
    }
}

/// 只有一个已知亡语时顺序不影响结果 → 正常结算,不报未支持。
#[test]
fn single_deathrattle_among_deaths_still_resolves() {
    let rules = MapRules::new(vec![deathrattle_card(
        "DR_A",
        vec![damage(Selector::EnemyHero, 1)],
    )]);
    // 21 没有规则 → 记覆盖缺口,但不计入亡语计数(未知不等于有)。
    let given = state_with(vec![(20, "DR_A", 1, 1, 3), (21, "NO_RULE", 1, 1, 3)]);
    let foe_hp_before = given.player(PlayerId(2)).unwrap().hero_health;

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::FriendlyMinions, 3)],
        TargetBinding::default(),
        &rules,
    );
    let then = expect_det(&out.outcome);

    assert_eq!(
        then.player(PlayerId(2)).unwrap().hero_health,
        foe_hp_before - 1
    );
    assert_eq!(
        out.unknown_rule_cards,
        vec![CardId::new("NO_RULE")],
        "查不到规则的死亡卡记入覆盖缺口"
    );
}

/// A4:连锁深度超限 → `Unsupported(RecursionLimit)`,不返回半截状态。
#[test]
fn death_chain_depth_limit_is_explicit() {
    // 一死带一死的链条:DR_i 的亡语对 DR_{i+1} 造成致死伤害。
    // 链条长于 MAX_DEATH_DEPTH(8),故必然撞上限。
    let depth = 12usize;
    let names: Vec<String> = (0..depth).map(|i| format!("DR_{i}")).collect();
    let mut programs = Vec::new();
    let mut minions = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let next = names.get(i + 1).cloned().unwrap_or_default();
        programs.push(deathrattle_card(
            name,
            vec![Operation::Damage {
                target: Selector::Filter(
                    Box::new(Selector::EntitiesInZone(Zone::Play)),
                    hs_effect_ir::EntityCondition {
                        card_id: Some(CardId::new(&next)),
                        ..Default::default()
                    },
                ),
                amount: 99,
            }],
        ));
        minions.push((20 + i as i32, name.as_str(), 1, 1, 3));
    }
    let rules = MapRules::new(programs);
    let given = state_with(minions);

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::Target, 3)],
        TargetBinding {
            source: None,
            target: Some(EntityId(20)),
        },
        &rules,
    );

    match &out.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::RecursionLimit) => {}
        other => panic!("期望 RecursionLimit,得到 {other:?}"),
    }
}

/// 召唤出的实体(stats unknown = 0/0)不应被死亡结算误杀。
#[test]
fn summoned_unknown_stats_entity_does_not_instantly_die() {
    let rules = MapRules::new(Vec::new());
    let given = state_with(Vec::new());

    let out = apply_with_rules(
        &given,
        vec![Operation::Summon {
            player: PlayerRef::SelfPlayer,
            card: hs_effect_ir::CardRef::Id(CardId::new("TOKEN_A")),
            count: 1,
        }],
        TargetBinding::default(),
        &rules,
    );
    let then = expect_det(&out.outcome);

    let token = then
        .entities
        .values()
        .find(|e| e.simulated)
        .expect("应召唤出实体");
    assert_eq!(token.zone, Zone::Play, "0/0(stats unknown)不等于已死");
    assert!(
        out.trace.entries.iter().all(|e| e.op != "death"),
        "不应触发死亡结算"
    );
}

/// A5:场上有光环卡 → 显式 `Unsupported(aura)`,trace 指名是哪张卡。
///
/// 光环未实装(`SimEntity` 无 buff 分层),静默按无光环算会让 M4 的斩杀判断
/// 出错,故整体不支持。`rules/` 现无 Aura* 卡,用自建 fixture。
#[test]
fn aura_on_board_is_explicitly_unsupported() {
    let mut aura_card = common::program("AURA_LORD", Vec::new());
    aura_card.status = RuleStatus::Partial;
    aura_card.confidence = 0.42;
    aura_card.build_range.max = Some(BuildId(99));
    aura_card.triggers = vec![hs_effect_ir::TriggerProgram {
        event: hs_effect_ir::TriggerEvent::AuraRecalculate,
        when: Vec::new(),
        effects: Vec::new(),
    }];
    let rules = MapRules::new(vec![aura_card]);
    let given = state_with(vec![(20, "AURA_LORD", 1, 2, 4)]);

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::EnemyHero, 1)],
        TargetBinding::default(),
        &rules,
    );

    match &out.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
            assert_eq!(what, "aura");
        }
        other => panic!("期望 aura 不支持,得到 {other:?}"),
    }
    let aura_note = out
        .trace
        .entries
        .iter()
        .find(|e| e.op == "aura")
        .expect("trace 应有 aura 条目");
    assert!(
        aura_note.detail.contains("AURA_LORD"),
        "应指名是哪张卡: {}",
        aura_note.detail
    );
    let aura_use = out
        .rule_uses
        .iter()
        .find(|rule_use| rule_use.card_id == CardId::new("AURA_LORD"))
        .expect("阻断执行的 aura 必须进入结构化规则证据");
    assert_eq!(aura_use.status, RuleStatus::Partial);
    assert_eq!(aura_use.confidence, 0.42);
    assert_eq!(aura_use.rule_version, "AURA_LORD@1..99");
}

/// 场上没有光环卡时不留 trace 噪音,正常执行。
#[test]
fn no_aura_leaves_no_trace_noise() {
    let rules = MapRules::new(vec![common::program("PLAIN", Vec::new())]);
    let given = state_with(vec![(20, "PLAIN", 1, 2, 4)]);

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::EnemyHero, 1)],
        TargetBinding::default(),
        &rules,
    );
    assert!(matches!(out.outcome, OutcomeSet::Deterministic(_)));
    assert!(
        out.trace.entries.iter().all(|e| e.op != "aura"),
        "无光环时不应记 aura 条目"
    );
}

/// A9:含死亡级联的执行同 seed 多次逐字节一致。
///
/// 死亡结算引入了新的遍历来源(死亡集合、亡语顺序),必须确定;
/// 收集走 `BTreeMap`/`BTreeSet` 序,`unknown_rule_cards` 按 CardId 升序。
#[test]
fn death_cascade_is_byte_identical_across_runs() {
    let rules = MapRules::new(vec![
        deathrattle_card("DR_AOE", vec![damage(Selector::FriendlyMinions, 9)]),
        deathrattle_card("DR_SECOND", vec![damage(Selector::EnemyHero, 5)]),
    ]);
    let given = state_with(vec![
        (20, "DR_AOE", 1, 2, 4),
        (21, "DR_SECOND", 1, 1, 5),
        (22, "NO_RULE_B", 1, 1, 5),
        (23, "NO_RULE_A", 1, 1, 5),
    ]);

    let run_once = || {
        let out = apply_with_rules(
            &given,
            vec![damage(Selector::Target, 4)],
            TargetBinding {
                source: None,
                target: Some(EntityId(20)),
            },
            &rules,
        );
        let state = format!("{:?}", expect_det(&out.outcome));
        let trace = format!("{:?}", out.trace.entries);
        let unknown = format!("{:?}", out.unknown_rule_cards);
        (state, trace, unknown)
    };

    let first = run_once();
    for i in 1..8 {
        assert_eq!(run_once(), first, "第 {i} 次执行与首次不一致");
    }

    // 覆盖率缺口按 CardId 升序,不受实体遍历顺序影响。
    assert_eq!(
        first.2,
        format!(
            "{:?}",
            vec![CardId::new("NO_RULE_A"), CardId::new("NO_RULE_B")]
        ),
        "unknown_rule_cards 应按 CardId 升序"
    );
}

/// 已知边界:亡语体里的随机操作显式不支持,措辞与「控制流嵌套随机」区分开。
///
/// 活例是警卫犬 `JAIL_878`(亡语=随机召唤一个 1 费亡语随从)。M3 定的
/// 「至多一个顶层随机、不得嵌套」边界在本任务内未放宽,故这类亡语不执行,
/// 而是显式报告 —— 见 PRD 已知降级。
#[test]
fn random_op_inside_deathrattle_is_explicitly_unsupported() {
    let rules = MapRules::new(vec![deathrattle_card(
        "DR_RANDOM",
        vec![Operation::RandomChoice {
            pool: hs_types::PoolId::new("some.pool"),
            count: 1,
        }],
    )]);
    let given = state_with(vec![(20, "DR_RANDOM", 1, 2, 4)]);

    let out = apply_with_rules(
        &given,
        vec![damage(Selector::Target, 4)],
        TargetBinding {
            source: None,
            target: Some(EntityId(20)),
        },
        &rules,
    );

    match &out.outcome {
        OutcomeSet::Unsupported(UnsupportedReason::NotImplemented { what }) => {
            assert_eq!(
                what, "random op in deathrattle",
                "措辞应与控制流嵌套随机区分,便于定位"
            );
        }
        other => panic!("期望亡语内随机显式不支持,得到 {other:?}"),
    }
}
