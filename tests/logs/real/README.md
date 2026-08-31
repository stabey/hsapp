# 真实对局日志 fixture

这些日志保留真实的游戏事件、顺序和结果，但所有 BattleTag 和
`GameAccountId` 都已统一替换为合成身份：PlayerId 1 使用 `PlayerA#0001` /
`[hi=1 lo=1001]`，PlayerId 2 使用 `PlayerB#0002` / `[hi=1 lo=1002]`。

前两份日志源自 HearthSim
[hsreplay-test-data](https://github.com/HearthSim/hsreplay-test-data)，并在入库前完成上述身份脱敏；
脱敏只改身份字段，不改游戏事件或测试结论。

| 文件 | 用途 |
|---|---|
| `186852_hearthstone_access_eoe.power.log` | M1 主验收：完整一局（build 185749，狂野天梯，打到 WON/LOST）重放确定性 |
| `friendly_player_id_is_1.power.log` | friendly_player 推断 ground truth（hslog 测试期望 = PlayerId 1） |

本地客户端日志同样必须先脱敏 BattleTag、`GameAccountId` 和用户路径后再入库。

| 文件 | 用途 |
|---|---|
| `2026-08-21-ranked-standard-loss.power.log` | 脱敏后的标准天梯完整对局（build 248348）：PlayerId 2 为友方并告负，PlayerId 1 获胜。 |

### `2026-08-21-ranked-standard-loss.power.log` 友方 / 胜者核对（M2 Track C，已定案）

管道（parse → normalize → reduce）在本局结束时给出：

| 角色 | PlayerId | 日志里的名字 | 依据 |
|---|---|---|---|
| 玩家 1 | `1` | 开局 `UNKNOWN HUMAN PLAYER`，结算行 `PlayerA#0001` | `CREATE_GAME`：`EntityID=2 PlayerID=1`；`DebugPrintGame`：`PlayerID=1, PlayerName=UNKNOWN HUMAN PLAYER`；终局 `TAG_CHANGE Entity=PlayerA#0001 tag=PLAYSTATE value=WON` |
| 玩家 2 | `2` | `PlayerB#0002` | `CREATE_GAME`：`EntityID=3 PlayerID=2`；`DebugPrintGame`：`PlayerID=2, PlayerName=PlayerB#0002`；终局 `PLAYSTATE=LOST` |
| 友方判定 | `2` | PlayerB | hslog 规则：第一个 `ZONE=HAND` 的 `SHOW_ENTITY` 归友方。本局该信号落到玩家 2 |
| 胜者 | `1` | PlayerA | 终局 `PlayerA#0001` → `WON`，`PlayerB#0002` → `LOST`。管道 `winner = PlayerId(1)` |

对应关系：`PlayerA#0001` → PlayerId 1 → winner；`PlayerB#0002` → PlayerId 2 → friendly_player。

该 fixture 的固定答案为 `friendly_player = PlayerId 2`、`winner = PlayerId 1`。
友方来自首个可见手牌信号，胜者来自终局 `PLAYSTATE`；
`hs-observed-state` / `hs-tracker` 用这两项作为回归断言。
