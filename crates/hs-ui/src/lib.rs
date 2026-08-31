//! `hs-ui` — GPUI 界面 token 与基础组件(M2 轻量视觉基础,PRD 决策 2)。
//!
//! 约束:UI 不维护第二套领域状态(设计文档 §23-9);
//! 领域状态经 ViewModel/消息接口注入,本 crate 不依赖 SQLite 与日志层。
//! 本 crate 是除 apps/desktop 外唯一允许依赖 GPUI 的 crate(M0 约定)。

pub mod components;
pub mod tokens;
