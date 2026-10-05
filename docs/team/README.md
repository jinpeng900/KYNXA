# KYNXA 五人开发分工

更新日期：2026-10-06。功能交接基线为 `0b3b23a`，包含本地 RAG、网页读取恢复和 Agent 基准；目录重组已提交。正式链路保持 WinUI → Node.js 网关 → 模型接口，以及网关 → C# ToolHost。

| 成员 | 主责 | 可分配目录 | 任务入口 |
|---|---|---|---|
| A | 编排、基础合同、集成与打包 | gateway orchestration/platform；desktop Services/Integration | [A](A-队长与任务编排.md) |
| B | WinUI/WebView2 展示与交互 | desktop Views/Controls/ViewModels/资源/Services/Presentation | [B](B-桌面前端.md) |
| C | 模型、协议、流式、预算、请求上下文 | gateway models；desktop Services/Models；shared Chat | [C](C-模型与本地推理.md) |
| D | 工具、权限、MCP/Skill、浏览器与原生执行 | gateway tools/official-tools；desktop Services/Tools；shared Tools；tool-host 功能目录 | [D](D-权限与工具执行.md) |
| E | 会话、记忆、索引、路径与迁移 | gateway data；desktop Services/Data；shared Memory | [E](E-数据与验证.md) |

gateway、desktop、shared、tool-host 分别指 apps/model-gateway、apps/desktop、apps/shared、apps/tool-host。实际依赖与证据见 [团队边界](../architecture/team-boundaries.md)，集成流程见 [队长总览](队长总览.md)，本轮实测见 [重组验证记录](../architecture/reorganization-validation.md)。

日常分工简化为 A 编排、B 桌面、C 模型、D 工具、E 数据五个主要模块；配套客户端和契约归同一领域负责人。GitHub 邀请命令、目录审查人生成与主分支保护见 [GitHub 协作](github-collaboration.md)。目录负责人是审查归属，不是 Git 的文件夹写入权限。

各人负责自己的实现、测试与说明；A 负责集成和工具设置 HTTP 接线，E 负责数据一致性，不承担全队测试。B 是全部 ShellPage partial 的唯一主负责人；多个 partial 共享页面状态，不代表完整 MVVM。

## 交接分支与具体文件

五个交接分支从同一个包含本说明的 `main` 提交建立。分支名称对应职责，不限制成员对目录的写入权限；每项后续任务建议从最新 `origin/main` 建独立任务分支，再向 `main` 提 PR。

| 成员 | 交接分支 | 典型文件与配套目录 |
|---|---|---|
| A 队长 | `codex/team-a-integration` | `orchestration/runtime.mjs`、`orchestration/tool-loop.mjs`、`orchestration/retrieval/coordinator.mjs`；`platform/`、desktop `Services/Integration/`、`Build/RuntimePackaging.targets`；根 CLI/工程入口、`tools/development/`、`.github/` |
| B 界面 | `codex/team-b-ui` | desktop `Views/ShellPage.*`、`Views/RetrievalSettingsWindow*`、`Resources/Transcript/transcript.js` / `transcript.css`；`Controls/`、`ViewModels/`、UI 模型/布局/资源及 `Services/Presentation/` |
| C 模型 | `codex/team-c-models` | `models/context.mjs`、`models/model-history.mjs`、`models/output-budget.mjs`、`models/streaming.mjs`、`models/retrieval/embedding-service.mjs` / `models/retrieval/reranker-service.mjs`；desktop `Services/Models/`、shared `Chat/`、`start-local-model.ps1` |
| D 工具 | `codex/team-d-tools` | `tools/mcp-client.mjs`、`tools/skill-service.mjs`、`tools/tool-policy.mjs`、`tools/browser-sessions.mjs`、`tools/retrieval/web-search.mjs`；`official-tools/`、tool-host `Desktop/Terminal/Sandbox/Native/`、desktop `Services/Tools/`、shared `Tools/` |
| E 数据 | `codex/team-e-data` | `data/conversations.mjs`、`data/memory-service.mjs`、`data/data-layout.mjs`、`data/retrieval/index.mjs` / `data/retrieval/source-library.mjs`；desktop `Services/Data/`、shared `Memory/`（含 `RetrievalApiContracts.cs`） |

表中未写 apps 前缀的网关路径以 `apps/model-gateway/` 为根，desktop/shared/tool-host 与上表定义相同。完整文件归属以 [主责清单](module-ownership.json) 为准；A 维护 `apps/mock-backend/`、`apps/desktop-preview/` 和公共构建配置，ToolHost 根 `Program.cs` 与工程文件也归 A，原生功能归 D。

## RAG 与网页搜索的协作边界

RAG 不另设第六位负责人，也不把所有检索代码集中给一个人：

| 成员 | 当前检索职责 |
|---|---|
| A | `orchestration/retrieval/`：请求路由、候选组合、证据缺口回读、HTTP 接线和任务生命周期 |
| B | 检索设置窗口、来源/引用展示、聊天与右侧面板接线 |
| C | `models/retrieval/`：嵌入与重排运行时、模型资源；请求上下文与工具观察预算 |
| D | `tools/retrieval/`：网页搜索、来源读取、工具描述；浏览器意图判断与执行适配 |
| E | `data/retrieval/`：SQLite 索引、分块、来源库、后台索引作业、证据引用和版本；检索 API 客户端与共享 DTO |

例如新增一个来源字段，由 E 维护共享数据合同，A 调整路由，B 接展示；更换嵌入模型由 C 维护推理与资源，E 配合索引版本，A 核对随包发布。不能只改一端，或者让界面另存一套正式来源记录。

当前已有正式聊天/确认记忆、三协议、流式工具循环、审批、MCP/Skill、浏览器、ToolHost 执行与本地混合检索；具体 RAG 能力见 [检索架构](../architecture/retrieval.md)。权限与隔离按 [工具架构](../architecture/agent-tools.md) 的实际实现说明。持久长任务、检查点、崩溃后自动恢复与确定性验证绑定属于后续工作，不能因目录重组或索引作业保存而宣称完成。

首轮交接目标是每人能定位主责源码、运行对应回归、按共享契约联调。旧独立 Host、Rust Authority 和六周排期不作为当前执行方案；后续按实际需求和团队容量安排。

## 主责清单与自动检查

[机器可读的模块主责清单](module-ownership.json) 维护每个目录/文件的唯一主责，不使用虚构 GitHub 账号。从仓库根运行：

```powershell
node tools/development/check-architecture.mjs
node tools/development/check-architecture.mjs --details
```

[依赖与归属守卫](../../tools/development/check-architecture.mjs) 检查静态相对引用、域依赖/循环和单一主责；`--details` 输出当前测试的主责，不把历史测试数量当成现有清单。守卫覆盖现有可识别语法，不检查计算生成的动态导入，也不代替功能测试。新增/移动模块同时更新清单与实际调用端。
