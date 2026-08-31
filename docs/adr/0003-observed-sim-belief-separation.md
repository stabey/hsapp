# ADR-0003: ObservedGameState / SimGameState / BeliefState 三态分离

- 状态：Accepted（2026-08-20）
- 关联：设计文档 §2.2、§4.1、ADR-007

## 背景

第一原则：日志描述已发生的真实世界，规则引擎预测可能世界，
Agent 在验证过的候选中做战略选择。三者混用会导致"推测污染事实"。

## 决策

- **ObservedGameState**（hs-observed-state）：仅含日志明确提供或
  确定性归约可得的信息，是 Tracker 与回放的权威状态；
- **SimGameState**（M3 引入）：规则引擎从某状态执行动作的产物，
  必须报告已验证/推断/未支持/采样；
- **BeliefState**（M4 引入）：不可见信息的概率描述，
  **绝不写回 ObservedGameState**；
- `reduce()` 为纯函数：同一事件序列 + 同一 Reducer 版本 = 同一状态。

## 替代方案

单一"游戏状态"对象 + 置信度字段 —— 被否：无法保证回放确定性，
推测值会渗入 UI 展示与回归测试基线。

## 后果

- 未实现规则显式返回 `Unsupported`，禁止静默猜测（ADR-015）；
- Reducer 可回放、可 Golden Test；
- 建议系统的所有输出必须绑定状态 revision。
