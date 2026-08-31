# 规则与池定义(M3)

- `cards/<card_id>.yaml` — 每张卡的 EffectProgram(`hs-effect-ir`);同卡多个
  build 版本用同文件多 YAML document(`---` 分隔)。
- `pools/<pool_id>.yaml` — 每个池的 PoolSpec(`hs-pools`),单 document。

写法注意(serde_yaml 0.9,详见任务 research/effect-ir-yaml-tags.md):

- 带字段的枚举变体用 YAML tag:`!damage`、`!card_type SPELL`、`!id CS2_033`;
  单位变体写普通字符串:`target`、`format_legal`、`uniform`。
- 触发条件:`when: [{subject: attacker, condition: {controller: self_player}}]`。

本目录被 CI 校验:`hs-effect-ir`/`hs-pools` 的 `repo_rules` 测试会解析
这里的全部文件,写错格式会在 `cargo test` 直接报错。

应用运行时从 `HS_RULES_DIR`(缺省 CWD 下 `rules/`)加载,Pools 页展示与求值。
