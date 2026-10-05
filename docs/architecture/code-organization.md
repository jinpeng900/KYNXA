# 当前代码组织与维护边界

更新：2026-10-05；已提交基线 `5637dc3`，本次重组在工作区。本文件描述已经运行的 C#/WinUI → Node.js 网关 → 模型接口及原生工具链。根 README 中的 Rust Authority、持久任务图和跨崩溃精确续跑仍是后续设计，不能用它们解释当前代码。

## 目录与职责

```text
apps/
├─ desktop/
│  ├─ Views/                 主窗口布局、事件接线、聊天与侧栏可见状态
│  ├─ Controls/              可复用控件、WebView2 聊天与图片预览
│  ├─ ViewModels/、Models/   展示对象、稳定身份和界面状态
│  ├─ Services/Integration/  A：网关启动、生命周期与通信基础
│  ├─ Services/Presentation/ B：展示、本地化与纯 UI 状态
│  ├─ Services/Models/       C：模型 API、流读取与预设
│  ├─ Services/Tools/        D：工具配置 API
│  ├─ Services/Data/         E：会话/记忆 API、路径与迁移
│  ├─ Resources/Transcript/  随包 Markdown/公式/代码和活动展示
│  └─ Build/                 独立运行时载荷、校验和许可证打包
├─ shared/Chat、Tools、Memory/ C/D/E：纯 DTO，不依赖窗口或网络
├─ model-gateway/
│  ├─ orchestration/         A：HTTP/工具设置路由、runtime、工具循环与生命周期
│  ├─ models/                C：协议、流、预算与上下文投影
│  ├─ tools/                 D：工具、权限、MCP/Skill 与执行器
│  ├─ data/                  E：会话、记忆、索引、路径与迁移
│  ├─ platform/              A 协调：跨模块纯合同与基础文件操作
│  └─ official-tools/        D：保留官方包位置
├─ tool-host/                D：Desktop/Terminal/Sandbox/Native；A 协调根入口与打包
├─ desktop-preview/         开发预览与 API 代理，不是第二份正式前端
└─ mock-backend/             旧合同的固定回复测试服务
tests/                       隔离数据、模拟模型和自有窗口的回归检查
.agents/skills/              开发仓库的 Skill，区别于应用模型技能
```

[团队边界与调用图](team-boundaries.md) 给出目录主责、共享改动流程和一手源码研究。根 server/initialize-storage/migrate-storage 保留入口，实现分别下沉 orchestration/data。Data 仅依赖自身及 Platform，Platform 不依赖高层；Models 的历史投影仍读取 Data 的公开工具结果，Tools 仍使用 Models 的纯辅助与 Data 服务，下层不反向导入 Orchestration。

目录名称不是独立性证明。`ShellPage.*` 仍是共享页面状态的 partial 类；`ProjectStore` 实际是会话 API 客户端。B 是全部 ShellPage partial 的唯一主负责人；客户端仍有 UiText 等展示类型依赖。不能因文件拆开就宣称已经完成完整 MVVM 或跨程序集隔离，也不能另建桌面正式日志绕开网关。

## 主要调用链

| 链路 | 所属模块与合同 |
|---|---|
| 发送、切换、流式展示 | Shell 接线 → Chat/Agent API 客户端 → `ChatStreamReader` → 展示状态；UI 只接收当前身份的结果 |
| 正式聊天与确认记忆 | 网关 `runtime` → `conversations` / `memory-service` → 仓储；JSONL 和来源版本保持权威 |
| 模型历史与预算 | `context`、`model-history`、`output-budget` 和模型协议适配；只压缩请求投影，调用与结果保持配对 |
| 工具执行 | `ToolService` 固定范围、参数与审批，分派文件/MCP/技能/原生执行，保存正式结果再继续模型循环 |
| 路径保护 | `ToolStorageBoundary` 解析统一存储身份和别名；它只判定路径，不授予执行权限 |
| 模型能力提示 | `buildToolSystemPrompt` 根据本请求的能力及技能快照生成有界提示；不会执行技能或改变配置 |
| 截图展示 | 正式工具引用 → `ConversationScreenshotSources` → 加载器 → 标签状态和图片控件；预览不能替代正式结果 |
| MCP 配置编辑 | 视图收集输入，`McpConfigurationInput` 做纯编辑校验，API 保存配置；后端仍负责最终校验 |
| 本机前后台窗口 | 原生启动/窗口操作 → `DesktopForegroundPlacement` → 真实观测回执；读取/截图与前台输入分开 |
| 背景浏览器参数 | `tools/desktop-launch-options.mjs` 在审批前准备 Chrome/Edge 的防遮挡限流参数；其他软件、明确前台模式和既有浏览器进程不改动 |
| 构建与部署载荷 | 项目声明资源清单，`Build/RuntimePackaging.targets` 处理独立 Node/ToolHost 和 PRI/许可流程 |
| 开发预览 API | `orchestration/server.mjs` 管静态文件与启动，`preview-api-proxy.mjs` 管流、背压与取消；正式数据仍归网关 |

## 既有修缮的边界

- 工具提示和存储路径策略从执行协调服务分离；预算、权限和来源 ID 保持原合同。
- 工具关闭失败仍清理其他所有者；模型运行时等候在途请求和已执行回执结算，错误保留给调用者。
- 非法工具输入返回有界错误，避免错误分支再次访问无效参数导致异常逃逸。
- Transcript 后台解析使用已捕获的缓存引用；关闭时使旧代次失效。注入的工具客户端由创建方释放。
- 闭合标签保留有界状态，短暂空投影不重开标签；浏览器布尔参数按实际值解析。
- 后台软件和独立终端保持原窗口状态，不以最小化换取不遮挡。截图取目标窗口，DOM/辅助功能读取与物理输入分开。
- 预览代理保留 API 查询、真实响应类型和增量输出；断开取消上游，坏 URI 返回错误而不终止服务。

终端不投影到正式右侧栏目；真实执行与协议事件、归档仍保留。独立终端控件及其隔离夹具不表示当前产品开启了右侧终端功能。

## 持续开发约束

公开网页读取另由 `tools/web-http-transport.mjs` 负责请求、地址校验、重定向、压缩、大小及取消，`tools/web-fetch.mjs` 复用上游解析器转换文本并生成短页和完整归档；官方目录仅引用声明，`ToolService` 继续拥有权限与回执。文件分页保留在文件模块。`tools/tool-system-prompt.mjs` 按运行时预留的声明空间选择技能摘要，未展示技能由原目录按需加载。

遵循 [开发 Skill](../../.agents/skills/kynxa-development/SKILL.md)、[代码书写 Skill](../../.agents/skills/kynxa-code-standards/SKILL.md) 和 [模块规范](../../.agents/skills/kynxa-development/references/coding-standards.md)。新行为放到实际拥有该职责的模块，不用任意行数上限驱动重写。

代码书写 Skill 以 Microsoft C#/.NET、Google JavaScript/TypeScript 与 PowerShell 官方指南为参考，维护本项目的语言选择、语义命名与双语注释规则；具体差异和一手来源集中在 [命名与双语注释](../../.agents/skills/kynxa-code-standards/references/naming-comments.md)。`.editorconfig` 提供格式与 C# 命名建议，不能代替兼容性审查。编写或审查本仓库代码时，`AGENTS.md` 自动引导同时使用两个 Skill。

本次规范化把预算、传输、取消和原生句柄的内部名称改得更明确，例如 `memoryBudgetTokens`、`stdoutBytes`、`_requestGate`、`inputDesktopHandle`，并保留原英文说明、补充中文。公开接口、JSON/schema 键、原生日志、工具 ID、XAML 绑定及第三方技能原文和许可证保持原合同；本次目录重组只调整官方包自有 Tools/catalog.mjs 的导入，不能宣称整个包逐字节不变；命名修改通过作用域和可执行语法树/token 对照检查，第三方许可证和上游内容不翻译。

异步 I/O 传递取消信号，UI 回写检查会话、请求和代次；后台任务观察错误。缓存有容量和失效规则，外部客户端、进程、事件和流由创建者释放。共享 DTO 不引用 WinUI，网关不依赖桌面程序集。

原始记录不因界面隐藏或上下文预算删除；身份、来源隔离、审批快照和版本冲突保持原合同。开发与回归只使用临时数据和模拟凭据。正式安装、真实账号浏览器、付费模型效果及所有第三方软件的前台行为需要各自验收，不由模拟测试代替。

验证命令见 [验证与协作](../../.agents/skills/kynxa-development/references/validation.md)、[网关结果](../../apps/model-gateway/README.md#当前验证与后续接口) 和 [打包说明](../../apps/desktop/Build/README.md)。代码组织、规范化和运行回归不是安全认证，也不表示已实现后续完整 Host 架构。

## 此前记录：2026-10-05 代码书写规范化验收

本节为此前规范化阶段记录，不是本轮五人目录重组验收。保留原结果与限制；本轮实际集成结果由负责人另行记录。

此次修改以内部命名、双语注释和开发规则为范围；验证使用独立临时数据、模拟模型和自有测试窗口。规范化通过语法树或非注释 token 对照检查，确认协议键、字面量、控制流和资源释放顺序保持兼容。另修正模型预设 smoke 工程遗漏的已有生产依赖链接，保留原断言。

| 此前实际检查 | 当时结果 |
|---|---|
| 桌面 x64 主构建、ToolHost 构建 | 0 警告、0 错误 |
| 完整网关套件，`--test-concurrency=4` | 670 项中 668 通过；2 项原生交互桌面能力检查失败 |
| 修改前后原生能力对照 | 同一会话分别编译 HEAD `9841c1d` 和现版；两者均 `available=false`、`interactiveWindows=false`，不是当时命名改动引入 |
| 真实 Transcript UI | 196 项通过，公式/表格、复制、实时语言、宽度、最终流式更新和会话滚动；当时未请求额外物理拖拽诊断 |
| 真实截图面板 UI | 132 项通过；Windows 拒绝夹具前台焦点，物理滚轮/拖拽/Escape 被明确跳过，未发送全局输入 |
| 原生工具管理 UI | 129 项通过，含配置、审批、结果分页、媒体、取消与晚到结果 |
| 便携运行时副本 | 39 项通过；空 PATH、隐藏系统 .NET、隔离网关与自包含 ToolHost |
| 客户端与纯逻辑 smoke | 流式、Agent、记忆、会话、HTTP 响应、扩展迁移、模型上下文/预设、35 项标签状态、22 项 Markdown 检查通过 |
| Native / preview | 24 项输入序列清理、9 项终端解码、4 项预览代理测试通过 |
| Skill | 两个 Skill 格式校验通过；独立示例 6 项检查通过，保留预算参数、JSON 键和取消回执顺序 |

两项网关失败及物理输入跳过仍是当时验收限制；不能将其描述为全套通过。第三方原文、许可证、发布哈希、真实用户数据及模型凭据不在此规范化范围中。
