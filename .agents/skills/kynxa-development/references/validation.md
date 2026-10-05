# 验证与协作

验证目标是本次用户行为及受影响约定。下列命令从仓库根目录执行，按改动选择；不要求每次全量运行。

## 选择检查

| 改动 | 优先验证 |
|---|---|
| 文案、样式、按钮等局部 UI | 桌面编译及相关实际界面状态；无需编写镜像实现的测试 |
| 侧栏布局、展开、排序、草稿 | `ui-layout-smoke`、`work-sidebar-smoke`；涉及 TreeView 增量更新再选 `project-tree-smoke` |
| 界面语言与即时切换 | `ui-language-smoke`，并检查活动窗口、表单/草稿和聊天正文 |
| Markdown、公式、代码、选择/复制/滚动 | `transcript-markdown-smoke` 和受影响的 `transcript-ui-smoke` 检查；高亮逻辑再选 `code-highlighting-smoke` |
| 流式 API、状态、取消/重试 | `chat-stream-smoke` 与对应网关 streaming/runtime/protocols 测试；改变主聊天表现再查 Transcript UI |
| 桌面 HTTP 响应、错误与资源所有权 | `gateway-response-smoke` 和受影响的模型/会话客户端 smoke，确认 JSON 默认值、409、取消及 SSE 仍兼容 |
| 模型预设、上下文配置 | `model-presets-smoke` 或 `model-context-smoke`，与对应 model-discovery/model-budget 测试 |
| 会话/记忆/归属/迁移/初始化 | 受影响 Node 测试及对应 C# 客户端 smoke，包含失败、恢复和并发边界 |
| 共享接口、构建文件、多模块核心改动 | 桌面编译、相关客户端和网关检查；影响面广时运行完整网关套件 |
| 开发 Skill/代理规则 | Skill 格式、YAML、引用路径、独立任务试用；无需为此启动产品或改真实数据 |

## 常用命令

桌面构建要求项目当前声明的 .NET/Windows SDK 与 WinUI 环境；网关 Node 版本以 [package.json](../../../../apps/model-gateway/package.json) 为准，目前 `>=22.19.0`。首次构建允许正常 restore，只有确认资产已恢复时才用 `--no-restore`。

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64
dotnet run --project tests/ui-layout-smoke/KYNXA.UiLayoutSmoke.csproj
dotnet run --project tests/work-sidebar-smoke/WorkSidebarSmoke.csproj
dotnet run --project tests/project-tree-smoke/ProjectTreeSmoke.csproj
dotnet run --project tests/ui-language-smoke/UiLanguageSmoke.csproj
dotnet run --project tests/transcript-markdown-smoke/TranscriptMarkdownSmoke.csproj
dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj
dotnet run --project tests/code-highlighting-smoke/CodeHighlightingSmoke.csproj
dotnet run --project tests/chat-stream-smoke/ChatStreamSmoke.csproj
dotnet run --project tests/gateway-response-smoke/GatewayResponseSmoke.csproj
dotnet run --project tests/model-context-smoke/ModelContextSmoke.csproj
dotnet run --project tests/model-presets-smoke/KYNXA.ModelPresetsSmoke.csproj
dotnet run --project tests/conversation-store-smoke/ConversationStoreSmoke.csproj
dotnet run --project tests/storage-migration-smoke/StorageMigrationSmoke.csproj
dotnet run --project tests/gateway-startup-smoke/GatewayStartupSmoke.csproj
node --test apps/model-gateway/tests/*.test.mjs
node tools/development/check-architecture.mjs
```

单项网关检查使用 `node --test apps/model-gateway/tests/实际文件名.test.mjs`。用 `rg --files apps/model-gateway/tests` 选择现存文件，不猜测测试名称。不要一次启动竞争相同端口、固定临时结果或窗口的多个 UI/集成测试。

主 Transcript UI 使用独立测试窗口和会话，结果在 `%TEMP%` 的测试文件中。旧 `markdown-ui-smoke`、`scroll-follow-smoke`、原生公式测试只验证兼容路径；它们通过不能代替当前 WebView2 聊天的复制和排版检查。

## 数据与可观察验收

- 测试用独立临时 Data、虚构连接与模拟上游，不能连接日常使用的数据根目录或消耗用户真实 API。不要打印原始凭据和私有聊天。
- 测试若会初始化 `StoragePaths` 或存储服务，在初始化前显式传入临时数据位置；桌面使用 `KYNXA_DATA_HOME`。不要依赖测试窗口自然隔离存储，也不要通过修改用户 `~/.kynxa/storage.json` 达到隔离。
- 优先验证外部行为：草稿未提交不保存、打开不改变排序、移动后范围正确、取消不污染下一聊天、冲突不丢数据；避免只断言函数内部每一步。
- 对状态型 UI 检查初始、选中、悬停/焦点、展开/收起和缩放等相关状态；不要仅凭编译通过声称操作已正确。
- 切换、关闭、异常与晚到结果属于相关生命周期验证。缓存/性能改动需观察其命中、失效与实际耗时，不能只以“用了缓存”作为改善证据。
- 通过必要检查后停止无依据的重复测试；新变更、失败或未解疑点才扩大验证。
- 记录实际执行命令、通过/失败及未验证原因。文档里的旧结果不是本次证据，环境问题也不能被描述成代码验证通过。

## 协作与交付

- 独立模块、只读审查和独立验证可委派，约定目标、相关约束、共享接口和写入文件归属，避免两个代理同时编辑同一文件。
- 不固定模型、推理强度或代理数量。默认继承当前会话，仅在用户明确要求时覆盖；此前“多开 Astra/ultra”不再是本项目开发要求。
- 主代理复核实际差异和接口兼容，运行必要集成检查。代理完成消息和单个模块测试不能自动证明整个功能成立。
- 检查用户已有改动后再编辑；必要时使用隔离工作树，但不强制每次创建。不用 `reset`、批量覆盖或清理来解决不理解的工作区状态。
- 相关行为/合同变更时更新现有维护文档；简单样式修改不用扩展架构说明，也不要求五人任务报告。
- 完成后说明具体行为、相关文件和验证结果。提交、推送、发布遵循已有授权，不重复请求已获许可，也不把代码修改授权扩大为发布授权。
