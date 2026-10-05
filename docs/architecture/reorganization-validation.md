# 五人目录重组验证记录

记录日期：2026-10-05。已提交基线为 `5637dc3`；本轮重组位于工作区，职责和依赖见 [团队边界](team-boundaries.md)。

本页只记录本轮实际执行并由集成负责人汇总的结果，区别于网关 README 和代码组织文档中的此前功能/规范化验收。没有执行的项目不记为通过，单独复测通过不改写首次完整套件的失败结果。

## 已执行结果

| 检查 | 本轮实际结果 | 范围与限制 |
|---|---|---|
| WinUI 桌面 x64 构建 | 首次及最终路由移动后第二次构建均 0 警告、0 错误 | 验证编译与资源引用，不替代全部 UI 操作 |
| 完整网关套件 | 670 项：669 通过，1 失败 | 失败为 mcp-connections.test.mjs 第 50 行删除临时目录时 EBUSY；首次全套不能标为全部通过 |
| 失败文件单独复测 | 13 项全部通过 | 为该 MCP 文件复测；不能替代重跑全套结果 |
| 纯 C# smoke | 15 个项目通过，1 个既有失败 | ui-language-smoke 缺少 UiText("关闭") 键；保留失败记录，本轮不扩大任务修改该产品文案 |
| 便携载荷 | 首次及路由移动后复测均 39 项通过 | 内置运行时和隔离执行检查；不等于正式安装发布或所有设备验收 |
| 图片面板 UI | 134 项通过 | 图片 UI 的实测范围，不替代主 Transcript |
| Agent UI | 129 项通过 | tools/设置/审批等实际 UI 检查，不替代 Transcript |
| Transcript UI | 首次夹具读取剪贴板失败；独立复测 196 项 DOM 检查通过 | 首次为 DataPackage 缺格式 COMException；未改生产源码，cold 694 ms、warm append 60 ms，未请求 pointer 检查 |
| HTTP 路由移动后回归 | 受影响 5 套测试共 40/40 通过 | HTTP/gateway/presets/skill package 等；不代替重跑全套 |
| 原生纯逻辑 | 输入序列 24 项、终端解码 9 项通过 | 本轮执行；纯逻辑不等同真实桌面输入/所有终端行为 |
| 网关可执行 AST 对照 | 72 个模块通过 | 扣除目录导入等必要接线后进行可执行结构对照，不构成功能测试或安全认证 |
| C# 移动源对照 | 67 个源文件字节一致 | 验证被移动源文件的内容保留；项目/引用接线仍需编译验证 |
| 依赖与主责守卫 | 240 个源码/工程、78 个模块、193 个静态相对引用，无违规/域循环 | 含 official-tools 自有 catalog；--details 提供 119 套测试主责 |
| 守卫故障注入 | 验证通过 | 注释/字符串内 import 不误报；Platform→Tools 被拒绝，域循环被检测 |

以上数字由本轮集成负责人提供；C# 与便携结果同时核对 artifacts/architecture-validation/desktop-smoke-results.json。已知项目入口包括 `apps/desktop/KYNXA.Desktop.csproj`、`tests/ui-language-smoke/UiLanguageSmoke.csproj`、`tests/screenshot-panel-ui-smoke/ScreenshotPanelUiSmoke.csproj`、`tests/native-desktop-smoke/InputSequenceSmoke.csproj` 和 `tests/native-host-terminal-smoke/HostTerminalDecoderSmoke.csproj`。

## 命令与证据

- 完整网关：`node --test --test-concurrency=4 apps/model-gateway/tests/*.test.mjs`。
- MCP 复测：`node --test apps/model-gateway/tests/mcp-connections.test.mjs`，13/13 通过。
- Agent UI：`tests/agent-ui-smoke/run.ps1`，129 项通过。
- 图片 UI：`tests/screenshot-panel-ui-smoke/run.ps1`，134 项通过。
- 依赖/归属：`node tools/development/check-architecture.mjs --details`。
- 路由复测：[http-route-retest.log](../../artifacts/architecture-validation/http-route-retest.log)，40/40 通过。
- Transcript 复测：[transcript-ui-retest.log](../../artifacts/architecture-validation/transcript-ui-retest.log)，196 项通过。
- 最终构建：[desktop-final-build.log](../../artifacts/architecture-validation/desktop-final-build.log)；便携复测：[node-runtime-final.log](../../artifacts/architecture-validation/node-runtime-final.log)。
- C#/便携汇总：[desktop-smoke-results.json](../../artifacts/architecture-validation/desktop-smoke-results.json)。日志在同一 ignored 目录；这些本地证据不随 Git 发布，其他检出需重跑。

C# 通过项目为 chat-stream、agent-client、memory-management、conversation-store、gateway-response、extension-storage、model-context、model-presets、work-tabs、transcript-markdown、ui-layout、work-sidebar、code-highlighting、storage-migration 和 gateway-startup smoke。ui-language-smoke 失败；汇总记录 ScreenshotViewerWindow 调用 UiText.Get("关闭")，原字典无该键的英文翻译。本轮未修改生产文案或测试断言。

## 验证限制

Transcript 首次剪贴板夹具失败与独立复测分别保留，未以复测抹掉首次结果；本轮未请求 pointer 检查。真实模型效果、真实 1M 上下文、已登录用户浏览器、正式安装发布与所有 Windows 设备不由上述检查保证。

本轮没有将 MCP 临时目录占用失败或既有语言键缺失隐藏为通过。持久长任务、检查点与崩溃恢复仍属后续能力；目录重组和现有回执不表示其已经完成。
