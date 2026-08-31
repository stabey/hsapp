# ADR-0004: 卡牌数据按 Hearthstone Build 版本化 + 自研 Migration Runner

- 状态：Accepted（2026-08-20）
- 关联：设计文档 §9、ADR-004/ADR-009

## 背景

卡牌数值、文本、卡池随补丁变化。历史录像必须用录像当时的 Build 数据
重放，不能被最新补丁覆盖。

## 决策

1. 卡牌相关表主键含 `build_id`：`PRIMARY KEY (build_id, card_id)`；
2. 导入完整卡牌集合（含英雄/技能/衍生物/Enchantment），
   `raw_json` 保留原文以便换数据源不改 schema（M0 拆解 D-04）；
3. 数据源接口可插拔，HearthstoneJSON 是首个实现而非唯一依赖；
4. **Migration 用自研轻量 runner**（hs-card-db::migrate，约 100 行）：
   `schema_migrations` 版本表 + 顺序 SQL + 每迁移一个事务 + 幂等；
   不引入 refinery 等框架（当前需求极简，可测试性优先；
   出现回滚/校验和需求再评估）；
5. 禁止应用启动时静默丢表重建（设计文档 §17.1）。

## 替代方案

- 仅按 card_id 存最新版 —— 被否：回放失真；
- refinery/sqlx-migrate —— 暂否：多一个依赖，收益不匹配。

## 后果

- 存储量随 Build 数增长（raw_json 每 Build 数十 MB），
  需要保留策略（M1+ 议题）；
- 新增迁移只能追加编号文件，禁止修改历史迁移。
