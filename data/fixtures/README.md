# Fixture 规范

本目录（及 `tests/logs/`）存放测试固定数据。所有测试禁止依赖网络。

## 命名

- 卡牌数据：`cards/<build_id>-<描述>.json`（如 `cards/999999-minimal.json`）
- 日志片段：`tests/logs/<build_id>-<场景>.powerlog.txt`
- 文件头注释注明来源与采集日期（JSON 用 `_meta` 键）

## 脱敏要求（提交前必查）

- 移除用户名路径（`/Users/<name>/...` → `<HOME>/...`）
- 移除 BattleTag、账号 ID、好友名
- 移除与测试目的无关的对局内容（最小化原则）

## 真实性等级

每个 fixture 标注两级之一：

- `real-sanitized`：真实客户端产物，脱敏后收录（优先）
- `synthetic`：手工构造，格式尽量贴近真实（过渡用，Track C 采集
  真实日志后应逐步替换）

`synthetic` 只描述 fixture 的组装方式，不表示其中的卡牌 ID、名称、文字或
游戏数据属于项目原创内容。只要使用真实游戏数据，`_meta` 必须同时写明
`source` 和 `rights`，并明确这些内容不受仓库 MIT 许可证覆盖。

## 卡牌 fixture 格式

采用 HearthstoneJSON 风格字段（`id`/`dbfId`/`type`/`cardClass`/`cost`/
`attack`/`health`/`collectible`/`name`/`text`），`name`/`text` 为
多 locale 对象。必须覆盖非 collectible 实体（英雄、英雄技能、衍生物、
Enchantment），因为完整引擎不只导入可收集卡（设计文档 §9.1）。
