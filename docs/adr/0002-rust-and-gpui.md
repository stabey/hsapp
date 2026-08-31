# ADR-0002: Rust + GPUI，官方 crates.io 精确版本锁定

- 状态：Accepted（2026-08-20，锁定机制修正于同日）
- 关联：设计文档 §3.1、M0 拆解 D-01

## 背景

核心、平台层与 UI 主体使用 Rust；UI 框架选定 GPUI（Zed 出品，GPU 加速）。
GPUI 处于 pre-1.0：2026 年 Zed 官方宣布聚焦 Zed 自身用例，
与 Zed 无关的外部需求（如本项目的透明 Overlay）不保证支持。

## 决策

1. **依赖来源**：Zed 官方发布到 crates.io 的 `gpui` crate，
   **精确版本锁定 `gpui = "=0.2.2"`**（`apps/desktop/Cargo.toml`）。
2. **决策修正说明**：原决策（D-01）为锁 Zed 仓库 git commit，前提是
   "官方无独立发版"。G0-02 实施时发现官方已在 crates.io 发版
   （repository 指向 zed-industries/zed），精确版本锁定与 commit 锁定
   同源同效且构建更快，故采用版本锁定。若未来需要未发版的修复，
   回退为 `git rev` 方式仍然可行。
3. **升级策略**：只在有明确收益时手动升级版本号，升级作为独立提交，
   必须重跑 GPUI 冒烟验证（`cargo run -p desktop --example gpui_smoke`）
   与 Overlay 探针验收项。
4. **隔离约束**：仅 `apps/desktop` 与 `hs-ui` 允许依赖 GPUI
   （`cargo tree -i gpui` 验证）；原生窗口操作集中在
   `hs-overlay-platform` / `hs-platform`。
5. **构建环境**：本机仅有 Command Line Tools（无 metal 编译器）时，
   启用 `runtime_shaders` feature 在运行时编译着色器。

## 替代方案

1. Zed 仓库 git commit —— 同源可行，保留为 fallback；缺点是拉全仓、
   无版本语义。
2. `gpui-unofficial` crates.io 系列 —— 非官方维护，被否。
3. `gpui-ce` 社区分支 —— 长期活性未知，被否。
4. Tauri/egui/SwiftUI —— 任务书明确禁止因 API 差异临时换框架。

## 后果

- GPUI 破坏性变更由精确版本锁定挡住；升级成本显式化；
- Overlay 场景踩到 GPUI 官方不维护的区域时，走 ADR-0005 降级路径；
- `runtime_shaders` 有轻微启动开销，发布版可在装有完整 Xcode 的
  CI 上改用预编译着色器（后议）。
