# ADR-0005: Overlay 原生适配层与 GPUI 降级路径

- 状态：Accepted（2026-08-20）
- 关联：设计文档 §14、ADR-006、M0 拆解 D-02

## 背景

Overlay 需要：透明、置顶、鼠标穿透、不抢焦点、跟随游戏窗口、
多显示器/DPI 正确。这些是 GPUI 最不成熟的区域（Zed 自身不用），
官方已声明不为外部用例投入。

## 决策

1. GPUI 只负责绘制/布局/动画；窗口级别、穿透、焦点、DPI 全部经
   `hs-overlay-platform::OverlayPlatform` trait 由平台代码实现
   （macOS: AppKit `NSPanel`/`ignoresMouseEvents`/window level；
   Windows: `WS_EX_NOACTIVATE|TOOLWINDOW|TOPMOST` 等）；
2. 平台调用禁止散落到 UI/业务 crate；
3. **降级路径（已获用户批准）**：若 Track A 探针证明 GPUI 无法满足
   Overlay 需求（拿不到可控 NSWindow/NSPanel、透明合成异常、穿透
   失效），则**主窗口继续用 GPUI，Overlay 窗口改为纯 AppKit +
   CALayer/Core Animation 自绘**；`OverlayPlatform` 接口不变，
   仅替换实现；不整体更换 UI 框架。
4. 触发判定：Track A 的 TA-06 探针报告必须给出 GO / NO-GO 结论。

## 替代方案

- 全局输入 Hook 实现局部穿透 —— 首版被否（权限与复杂度）；
- 注入游戏渲染管线实现独占全屏覆盖 —— 永久禁止（产品边界）。

## 后果

- Overlay 能力上限受平台窗口系统约束（Windows 独占全屏不承诺）；
- 接口先行使降级成本可控（只换实现层）。
