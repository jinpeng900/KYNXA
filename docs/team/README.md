# KYNXA 五人开发分工

更新日期：2026-10-22，当前为 Windows x64 首个正式版 1.7（安装发行版本 `1.7.0`）。当前正式链路为 WinUI/WebView2 → Windows 原生 Rust 网关 → 模型接口、Rust 推理/工具/存储服务及 C# ToolHost；独立资源服务提供真实硬件准入。入口为 `apps/agent-core/src/bin/kynxa-gateway.rs`，随包路径为 `runtime/agent/kynxa-gateway.exe`，公开 HTTP/SSE 保持桌面合同。WebView 界面 JavaScript 保留，产品后端不依赖 Node/NAPI；私有 Node 只随包服务扩展 MCP 和已核验的 JS Skill。实际范围见 [原生架构](../architecture/native-gateway.md)，本版变化见 [1.7 说明](../releases/1.7.md)，全部维护入口见 [文档索引](../README.md)。

| 成员 | 主责 | 可分配目录 | 任务入口 |
|---|---|---|---|
| A | 编排、基础合同、集成与打包 | agent-core gateway/组合/orchestration/resources；desktop Services/Integration | [A](A-队长与任务编排.md) |
| B | WinUI/WebView2 展示与交互 | desktop Views/Controls/ViewModels/资源/Services/Presentation | [B](B-桌面前端.md) |
| C | 模型、协议、流式、预算、请求上下文 | agent-core models；desktop Services/Models；shared Chat | [C](C-模型与本地推理.md) |
| D | 工具、权限、MCP/Skill、浏览器与原生执行 | agent-core tools；official-tools；desktop Services/Tools；shared Tools；tool-host 功能目录 | [D](D-权限与工具执行.md) |
| E | 会话、记忆、索引、路径与迁移 | agent-core retrieval/storage；desktop Services/Data；shared Memory | [E](E-数据与验证.md) |

agent-core、official-tools、desktop、shared、tool-host 分别指 apps 下的同名目录。实际依赖与证据见 [团队边界](../architecture/team-boundaries.md)，集成流程见 [队长总览](队长总览.md)。[重组验证记录](../architecture/reorganization-validation.md) 保留旧阶段证据，其中 Node 测试成绩和路径不代表当前原生网关验收。

日常分工简化为 A 编排、B 桌面、C 模型、D 工具、E 数据五个主要模块；配套客户端和契约归同一领域负责人。GitHub 邀请命令、目录审查人生成与主分支保护见 [GitHub 协作](github-collaboration.md)。目录负责人是审查归属，不是 Git 的文件夹写入权限。

各人负责自己的实现、测试与说明；A 负责集成和工具设置 HTTP 接线，E 负责数据一致性，不承担全队测试。B 是全部 ShellPage partial 的唯一主负责人；多个 partial 共享页面状态，不代表完整 MVVM。

## 交接分支与具体文件

五个交接分支从同一个交接基线建立。分支名称本身不授予权限。已记录的界面协作约定是 `hui33844` 使用 `kynxa_team/b-ui`，写入限制见 [远端规则集历史记录](rulesets/README.md)；实际邀请和规则状态须另行核验，不能以本次文档更新声称仍然生效。界面修改由队长在独立副本测试，再决定是否合并到 `main`，不要求界面成员 Fork 或创建额外任务分支。

| 成员 | 交接分支 | 典型文件与配套目录 |
|---|---|---|
| A 队长 | `kynxa_team/a-integration` | agent-core `src/gateway.rs`、`gateway_contract.rs`、`bin/kynxa-gateway.rs`、`orchestration/gateway_runtime.rs` / `gateway_loop.rs`、`resources/`；desktop `Services/Integration/`、`Build/RuntimePackaging.targets`；工程与开发检查、`.github/` |
| B 界面 | `kynxa_team/b-ui` | desktop `Views/ShellPage.*`、`Views/RetrievalSettingsWindow*`、`Resources/Transcript/transcript.js` / `transcript.css`；`Controls/`、`ViewModels/`、UI 模型/布局/资源及 `Services/Presentation/` |
| C 模型 | `kynxa_team/c-models` | agent-core `src/models/gateway.rs`、`gateway_protocol.rs`、`gateway_stream.rs`、`gateway_config.rs`、`gateway_context.rs`、`gateway_inference.rs` 与纯协议/预算/历史状态；desktop `Services/Models/`、shared `Chat/` |
| D 工具 | `kynxa_team/d-tools` | agent-core `src/tools/service_*` 与 `gateway_mcp.rs` / `gateway_browser.rs` / `gateway_web.rs` / `gateway_process.rs`；official-tools 静态清单/Skills、tool-host `Desktop/Terminal/Sandbox/Native/`、desktop `Services/Tools/`、shared `Tools/` |
| E 数据 | `kynxa_team/e-data` | agent-core `src/storage/gateway.rs` / `gateway_memory.rs` / `conversation_history.rs` 与正式日志/事务，`src/retrieval/gateway_index.rs` / `gateway_embedding.rs` / `gateway_documents.rs` / `gateway_structure.rs` / `gateway_tools.rs`；desktop `Services/Data/`、shared `Memory/`（含 `RetrievalApiContracts.cs`） |

完整文件归属以 [主责清单](module-ownership.json) 为准；A 维护 `apps/mock-backend/`、`apps/desktop-preview/` 和公共构建配置，ToolHost 根 `Program.cs` 与工程文件也归 A，原生功能归 D。预览服务和 Node 架构检查仅用于开发，不随正式后端发货。`src/bin` 是实际 Rust 源码入口，不因通用 `bin` 排除规则漏掉。

桌面由 `Services/Integration/ModelGatewayService.cs` 启动随包原生网关。开发手动启动使用 `tools/development/start-native-gateway.ps1`，先构建桌面及原生随包资产；测试必须传独立 Data、模型/扩展目录和端口，不能启动另一个网关写日常数据。

当前随包与 SSE 合同验证使用 `tests/native-runtime-smoke`、`tests/native-gateway-contract-smoke`，完整参数见 [验证参考](../../.agents/skills/kynxa-development/references/validation.md)。旧 Node 启动/打包套件及 Agent、SciFact、统一智能体执行适配器属于历史入口，尚未完成原生评测适配；归档保留原题集、评分与报告，不把旧执行成绩称为当前 Rust 验收。独立客户端、WinUI/WebView 检查与开发架构守卫继续保留。

## RAG 与网页搜索的协作边界

RAG 不另设第六位负责人，也不把所有检索代码集中给一个人：

| 成员 | 当前检索职责 |
|---|---|
| A | `orchestration/gateway_loop.rs` 与根 HTTP 接线：请求路由、候选组合、证据缺口回读和任务生命周期 |
| B | 检索设置窗口、来源/引用展示、聊天与右侧面板接线 |
| C | `models/gateway_inference.rs`：真实 ORT E5 嵌入与固定资产空间；模型上下文和观察预算；检索使用词法/E5 RRF 与轻量排序，不再随包运行 BGE |
| D | `tools/service_source_reading.rs`、网页/浏览器服务：来源扫描、权限与原生取消、工具描述和执行 |
| E | `retrieval/` 与 `storage/`：SQLite/向量索引、分块、来源库、后台索引作业、资源租约与证据版本；检索 API 客户端与共享 DTO |

例如新增一个来源字段，由 E 维护共享数据合同，A 调整路由，B 接展示；更换嵌入模型由 C 维护推理与资源，E 配合索引版本，A 核对随包发布。不能只改一端，或者让界面另存一套正式来源记录。

当前源码覆盖正式聊天/确认记忆、OpenAI Chat/Responses、Anthropic、Ollama、流式工具循环、审批、MCP/Skill、浏览器、ToolHost 与本地混合检索；实际能力与验收分别见对应实现和本轮结果，编译不替代复现。具体 RAG 能力见 [检索架构](../architecture/retrieval.md)，权限与隔离见 [工具架构](../architecture/agent-tools.md)。已有任务回执、上下文检查点和重启后事实读取不能被描述为任意副作用自动安全重放。

首轮交接目标是每人能定位主责源码、运行对应回归、按共享契约联调。旧独立 Host、Rust Authority 和六周排期不作为当前执行方案；后续按实际需求和团队容量安排。

## 主责清单与自动检查

[机器可读的模块主责清单](module-ownership.json) 维护每个目录/文件的唯一主责，不使用虚构 GitHub 账号。从仓库根运行：

```powershell
node tools/development/check-architecture.mjs
node tools/development/check-architecture.mjs --details
```

[依赖与归属守卫](../../tools/development/check-architecture.mjs) 检查源码、静态官方资源、域依赖/循环、精确依赖版本和单一主责。纯规则不得引用 I/O 集成层；`rustApps[].integrationModules` 逐文件限制实际跨域接线，`executionModules` 逐文件标明真实执行。`--details` 输出当前测试主责，不把历史测试数量当成现有清单。守卫覆盖可识别静态语法，不代替编译或功能测试；新增/移动模块同时更新清单与实际调用端。
