# 第三方声明

本 crate 内嵌以下仅用于 Android UI 自动化的二进制资源：

- `u2.jar` 0.4.0，来源于 openatx/android-uiautomator-server-jar，MIT License。
- `app-uiautomator.apk` 2.4.0，来源于 openatx/android-uiautomator-server，MIT License。

上游 APK 包含 AndroidX 组件。AndroidX 使用 Apache License 2.0；完整依赖和
对应 notice 以 APK 内 `META-INF` 文件及上游构建清单为准。

`u2.jar` 包含或派生了 Genymobile scrcpy 的设备交互实现。scrcpy 使用 Apache
License 2.0，版权归 Genymobile 及其贡献者所有。

MIT License 和 Apache License 2.0 的原文分别见：

- https://opensource.org/license/mit
- https://www.apache.org/licenses/LICENSE-2.0

资源的固定大小、SHA-256、来源和版本记录在 `assets/agents.json`。
