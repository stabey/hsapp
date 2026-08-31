# ADR-0001: 独立跨平台桌面应用，不做 HDT 插件

- 状态：Accepted（2026-08-20）
- 关联：设计文档 §2、ADR-001

## 背景

需要在 macOS 和 Windows 上提供 Deck Tracker 与后续智能建议。现有生态：
HDT（Windows/.NET/WPF）、HSTracker（macOS/Swift）。HDT 插件机制依赖其
Windows 运行时，无法作为跨平台基础。

## 决策

开发独立桌面应用：读取客户端日志 → 观察状态 → Tracker/Overlay →
（后续）建议系统。只读取客户端可见日志，不自动操作游戏、不注入进程、
不读取隐藏信息。

## 替代方案

1. HDT 插件 —— 被否：绑死 Windows/.NET，无法覆盖 macOS 首发目标。
2. 分别 fork HDT 与 HSTracker —— 被否：两套代码库双倍维护，
   核心资产（规则引擎、池系统）无法共享。

## 后果

- 需要自建日志解析、卡牌数据、Overlay 全链路（工作量大但资产自有）；
- 平台差异集中到 hs-platform / hs-overlay-platform 两个适配层；
- 产品边界（只读日志）成为长期合规与信任基础。
