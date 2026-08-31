# Overlay 技术探针报告（M0 Track A / TA-06）

日期：2026-08-20
环境：macOS 25.6.0（darwin），gpui 0.2.2（crates.io，runtime_shaders），Rust 1.97.1

## 结论：GO（已确认，2026-08-20）

GPUI 可以承担 Overlay 窗口的创建与绘制，AppKit 行为通过原生句柄补齐，
**不触发** ADR-0005 的纯 AppKit 降级路径。三个真机视觉场景已全部人工
复核通过：

1. Passive 穿透 + 不抢焦点 ✅
2. Interactive 拖动 + 位置记忆 ✅
3. 全屏 Space 可见 ✅（以全屏 Cursor 窗口验证，Overlay 悬浮其上）

## 依据

### 1. GPUI 提供了 Overlay 需要的全部窗口原语（源码级确认）

对 gpui 0.2.2 源码（`platform/mac/window.rs`）核查：

- `WindowKind::PopUp` 创建的是 **NSPanel 子类**（`GPUIPanel`），并自动附加：
  - `NSWindowStyleMaskNonactivatingPanel`（点击不激活应用，不抢游戏焦点）
  - `NSPopUpWindowLevel`（初始置顶）
  - `CanJoinAllSpaces | FullScreenAuxiliary` collectionBehavior
    （可出现在全屏 Space 上——这是全屏炉石场景的关键前提）
- `WindowBackgroundAppearance::Transparent` 提供透明背景
- `Window` 实现 `raw_window_handle::HasWindowHandle`，可拿到 `ns_view`
  → `[view window]` 得到 NSWindow，**原生句柄可控**（ADR-0005 的
  GO/NO-GO 判据即此项）

### 2. GPUI 未暴露、由 hs-overlay-platform 补齐的行为

| 行为 | AppKit 调用 | 备注 |
|---|---|---|
| 鼠标穿透 | `setIgnoresMouseEvents:` | Passive 模式 |
| 背景拖动 | `setMovableByWindowBackground:` | Interactive 模式 |
| 层级切换 | `setLevel:`（ScreenSaver=1000 / PopUp=101 / Normal=0） | Passive 用 1000 盖全屏游戏 |
| 显隐 | `orderFrontRegardless` / `orderOut:` | Hidden 模式 |
| 跟随 | `setFrame:display:`（point 坐标，顶左→底左换算） | 8Hz 轮询 |
| 失活不消失 | `setHidesOnDeactivate:NO` | **NSPanel 默认 YES，不关会在玩家点回游戏时消失，必须覆盖** |
| 阴影/不透明 | `setHasShadow:NO` + `setOpaque:NO` | |
| Exposé 固定 | collectionBehavior 追加 `Stationary` | |

### 3. 运行时验证（已通过）

- `cargo run -p desktop` 启动主窗口（导航壳 + Overlay 控制区），持续运行无 panic
- Overlay 窗口按保存位置/默认位置创建，原生句柄获取与 configure 成功
- `cargo test -p hs-platform`：CGWindowList 枚举在本机返回 layer==0 窗口列表不崩溃
- fmt / clippy(-D warnings) / test 三绿（Track A 涉及的 3 个 crate）

### 4. 坐标体系结论（TA-05）

- CGWindowList 返回**全局 point 坐标（顶左原点）**，非物理像素；
  跟随链路统一走 point（`set_frame_points`），避免物理像素往返换算误差
- NSWindow frame 为底左原点 point，换算基准是 `NSScreen.screens[0]` 高度
- 物理像素契约（`OverlayPlatform::set_bounds(PhysicalRect)`）保留，
  按窗口 `backingScaleFactor` 换算；双显示器混合 DPI 的精确换算
  留待 M1 真机双屏验证
- 读取窗口 owner 名/边界不需要屏幕录制权限（刻意不读 `kCGWindowName`）

## 人工复核记录（三场景，均已通过）

1. **普通窗口**：Passive 穿透、不抢焦点；Interactive 可拖、位置记忆 ✅
2. **游戏最大化**：Overlay 保持在上方、切换应用不消失 ✅
3. **全屏 Space**：以全屏 Cursor 验证，Overlay 出现在全屏 Space 上 ✅
   （FullScreenAuxiliary + CanJoinAllSpaces + level 1000 组合有效）

已知限制 / 风险：

- 全屏 Space 场景下 level 1000 与系统 UI（通知中心等）的层级竞争未验证
- `runtime_shaders` 首帧着色器编译延迟未测量（对 Overlay 影响预计可忽略）
- 若场景 3 失败：降级不是全盘 AppKit 重写，而是仅对 Overlay 窗口改用
  AppKit 手建 NSPanel + CALayer 绘制（主窗口保持 GPUI），估算 3~5 天
