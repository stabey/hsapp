# python-hslog 差分对照（M1 R5）

同一份 Power.log 分别交给 `hs-log-core` 的 `PowerLogParser` 和 [HearthSim/python-hslog](https://github.com/HearthSim/python-hslog)，对照 Packet **种类序列**。

本工具是迁移期 Oracle：**差异需人工判读，不作硬门槛，不进 CI。**

## 对照口径

只比较下列种类（与 `Packet` 变体对齐，不含 `Unknown`）：

`CreateGame` / `FullEntity` / `ShowEntity` / `ChangeEntity` / `HideEntity` /
`TagChange` / `BlockStart` / `BlockEnd` / `Options` / `Choices` /
`ChosenEntities` / `SendOption` / `SendChoices`

第一版输出：

1. 两侧种类序列（不一致位置 + 各自 ±3 条上下文）
2. 每种 Packet 计数对照表
3. `TagChange` 的 `(entity, tag, value)` 多重集合抽样（仅 rust-only / hslog-only）

不做字段级全等：两侧结构不同（hslog 是 packet 树 + hearthstone 枚举；我们是扁平 `Packet` + `RawTag`）。

### GameState 范围

- **我们**：只消费 `GameState.*`（`DebugPrintPower` / `DebugPrintOptions` / `SendOption` / `DebugPrintEntityChoices` / `DebugPrintEntitiesChosen` / `SendChoices`）。`PowerTaskList.DebugPrintPower` 与 GameState 镜像，解析为 `Unknown` 以免重复。
- **hslog**：默认 `_game_state_processor = "GameState"`，同样只回调 `GameState.*` 方法；`PowerTaskList.*` / `LoadingScreen.*` 被静默丢掉。
- **diff**：Rust 侧丢掉 `Unknown`，hslog 侧丢掉 `MetaData` / `ResetGame` / `SubSpell` / `VOSpell` / `ShuffleDeck` / `CachedTagForDormantChange` / `CreateGame.Player` / `Option` 等非对照种类。`Block` 摊平为 `BlockStart` + 子包 + `BlockEnd`；`SubSpell` 本身不计，但继续递归其子包。

## 输入要求

hslog 的 `TIMESTAMP_RE` 要求行形如：

```text
D 21:35:47.4835077 GameState.DebugPrintPower() - CREATE_GAME
```

`tests/logs/999999-game-open.powerlog.txt` 已带此前缀。`run.py` 会：

- 跳过空行与 `#` 注释（fixture 头）
- 对**没有** `D|W|E` 时间戳前缀的行补 `D 00:00:00.0000000 `，以便手工截取的 GameState 行也能喂给 hslog

Rust 侧 `dump_packets` 不依赖前缀：`PowerLogParser` 按行内 `GameState.*() - ` 标记识别。

## 建立 venv

在仓库根目录：

```bash
python3 -m venv tests/differential/venv
source tests/differential/venv/bin/activate   # Windows: tests\differential\venv\Scripts\activate
pip install -r tests/differential/requirements.txt
```

`venv/` 已 gitignore，不要提交 site-packages。

包名是 PyPI 上的 `hslog`（即 python-hslog）。

## 运行

单独导出 Rust NDJSON：

```bash
cargo run -p hs-log-core --example dump_packets -- tests/logs/999999-game-open.powerlog.txt
```

对照（无 `--rust-ndjson` 时脚本自己调 `cargo run`）：

```bash
python3 tests/differential/run.py tests/logs/999999-game-open.powerlog.txt
```

使用预先生成的 NDJSON：

```bash
cargo run -p hs-log-core --example dump_packets -- tests/logs/999999-game-open.powerlog.txt > /tmp/packets.ndjson
python3 tests/differential/run.py tests/logs/999999-game-open.powerlog.txt --rust-ndjson /tmp/packets.ndjson
```

真实对局日志同样用法，把路径换成采集到的 `Power.log`。

## 输出解读

- `totals`：`rust_all` 含 `Unknown`；`rust_cmp` / `hslog_cmp` 是对照集合。`hslog_skipped` 是 hslog 有而我们不对照的种类计数。
- `kind counts`：`delta = rust - hslog`，非 0 行标 `*`。
- `kind sequence: MATCH` 表示对照种类顺序一致。不一致时打印区域起点及两侧 ±3。
- `TagChange ... sets: MATCH` 表示 `(entity, tag, value)` 多重集合一致。抽样最多 20 条，仅作线索。
- 退出码：`0` 种类序列一致；`1` 序列不一致或解析失败；`2` 文件/依赖缺失。退出码**不是** CI 门槛。

手工 fixture 有差异是正常的：它为 golden 测试构造，不是完整对局，hslog 也可能对残缺 Options/Choices 更严。

## 已知口径差异

| 项 | 我们 | python-hslog |
|---|---|---|
| PowerTaskList / LoadingScreen | `Packet::Unknown`（保留行） | 无回调，丢弃 |
| 对照前的行 | 全部喂给 parser | `CREATE_GAME` 之前的消息忽略 |
| Block | 扁平 `BlockStart` / `BlockEnd` | `Block` 树，脚本摊平后再比 |
| SubSpell / META_DATA / RESET_GAME / VO_SPELL / SHUFFLE_DECK | 多为 `Unknown` 或未建模 | 独立 packet；diff 默认剔除（SubSpell 子包仍进入序列） |
| Tag / value | `game_tag` 映射 + 原文 `tag_name`/`value_raw` | `hearthstone.enums`；未知枚举可能报错 |
| `Choices.tasklist` | 契约无此字段，丢弃 | 保留 |
| `SendOption.id` | 日志常缺，缺省 `0` | 无 id 字段 |
| Option target error | 只留 `Vec<EntityId>` | per-target error/errorParam |
| `ShowEntity`/`HideEntity`/`ChangeEntity` entity 解析失败 | `entity_id=None` + `entity_raw` 保留（与 BlockStart 一致） | 可能是 `None` / `PlayerReference` |
| 字段结构 | serde 外部标签 + `records` | 对象树，无 NDJSON 契约 |
| 时间戳前缀 | 可选 | 必需（脚本可补占位） |
| 玩家名实体引用（`Entity=Foo#123`） | parser 层为 `EntityId(0)`+`entity_raw`，**normalizer 层**按名字/排除法解析 | parser 层经 PlayerManager 直接解析 → TagChange 多重集里表现为 rust 侧 entity=0 与 hslog 侧 entity=2/3 的对称差异，属口径差异非 bug |
| `GameState.DebugPrintGame()` | `Packet::GameMetadata`（rust-only 种类，不进可比集合） | 树的元数据，不产 packet |

## 战果记录

2026-08-21 对 `tests/logs/real/186852_hearthstone_access_eoe.power.log` 首跑,
differential 抓出三个真 bug（均已修复）:

1. 行尾空格未 trim → `value=WON ` 匹配失败,STEP/PLAYSTATE 全归 0;
2. 数字形式 tag 名（`tag=467`）与未收录具名 tag（NEXT_STEP 等）全部塌缩为 0,
   实体 tags 互相覆盖 → 引入 `game_tag_table.rs` / `game_tag_value_table.rs`
   全量生成表（生成命令见下）;
3. M0 手写 `step` 常量从 MAIN_READY 起整体错位（17,6,7…应为 6,7,8…）。

### 生成表再生成命令

```bash
source tests/differential/venv/bin/activate
python3 tests/differential/gen_tag_tables.py
```

## 依赖说明

- Rust 导出只需已有 workspace；`dump_packets` 通过 `hs-log-core` 的 **dev-dependency** `serde_json` 序列化（不改 `src/`）。
- Python 侧需要能 `import hslog`。无网络时把脚本和本 README 就绪即可，不要为了安装去改 CI。
