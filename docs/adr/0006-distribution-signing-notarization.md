# ADR-0006: 分发签名与公证（占位）

- 状态：Proposed（2026-08-20，M2 前必须定稿）
- 关联：M0 拆解 D-07

## 背景

一个常驻、置顶、透明覆盖窗口的 macOS 应用，未经 Developer ID 签名 +
公证（notarization）无法正常分发给用户（Gatekeeper 拦截）。
Windows 侧同理需要代码签名以避免 SmartScreen 拦截。

## 初步方向（待定稿）

- macOS：Developer ID Application 证书 + `notarytool` 公证 +
  stapler；打包 DMG 或 zip；自更新候选 Sparkle 2；
- Windows：Authenticode 签名（证书来源与成本待调研）；
- CI 出包流水线在 M2 前建立；
- 本 ADR 在 M2 规划时补全并转 Accepted。

## 当前状态（2026-08-28 更新）

自签打包流水线已建立，**公证与 Developer ID 签名仍未做**，本 ADR 因此保持 Proposed。

已有：

- `cargo xtask bundle [--release]` 产出 `HearthAdvisor.app`
  （`CFBundleIdentifier` = `dev.stabey.hearthadvisor`，ad-hoc 自签，自带结构/plist/
  规则/签名四项自检）；
- CI 每次构建跑一次打包冒烟；手动触发或推 `v*` 标签时产出可下载的发布版 .app 压缩包；
- 用法与规则目录定位见 README「打包为 macOS 应用」。

仍缺（下一轮定稿时处理）：

- Developer ID Application 证书签名与 `notarytool` 公证 —— 需要 Apple 付费开发者账号
  与 CI 密钥保管，是本 ADR 转 Accepted 的前提；
- DMG 打包与自更新（Sparkle 2 候选）；
- Windows Authenticode 签名。

## 当前影响

自签产物仅供本机与开发者自用：别人下载会被 Gatekeeper 拦截，需手动放行。
另外，自签封条随二进制内容变化，重新打包后 macOS 可能视其为另一个应用，
已授予的 TCC 权限需重新勾选；固定输出路径可减少这种情况，根治需 Developer ID 证书。
