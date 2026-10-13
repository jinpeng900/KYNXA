# 代码与模块规范

本文件是开发约束，不要求按目录模板重建项目。以当前调用链为准，相关改动逐步改善职责。

验证范围和交付节奏统一按[最低必要验证与推进节奏](validation.md#最低必要验证与推进节奏)执行；以主流程可用为先，不为形式规范、假设性边界或局部修改扩成整仓整理与完整验收。

## 模块职责与依赖

当前正式链路：WinUI/C# 桌面 → Windows 原生 `kynxa-gateway.exe` → 已配置模型接口、Rust 推理/工具/存储服务及 C# ToolHost。入口在 `apps/agent-core/src/bin/kynxa-gateway.rs`，HTTP/SSE 在 `src/gateway.rs`；任务、恢复、资源准入、真实模型 HTTP/ORT 推理与正式日志/SQLite/资料事务均由 Rust 执行。正式写入仍经单网关的串行事务，桌面不新增写入者。读取 [Rust 接线](../../../../docs/architecture/native-gateway.md)，按实际函数区分业务规则与 I/O 生命周期，不再新增 Node/NAPI 接线或后端 JavaScript 回退。

五人职责及实际目录见 [模块边界](../../../../docs/architecture/team-boundaries.md)。Rust 的 orchestration/resources 与组合根归 A、models 归 C、tools 归 D、retrieval/storage 归 E；静态官方工具资源位于 `apps/official-tools/`。桌面 Services 对应 Integration/Presentation/Models/Tools/Data；Shared 按 Chat/Tools/Memory 合同分目录；ToolHost 按 Desktop/Terminal/Sandbox/Native 分目录。B 维护全部 ShellPage partial。`node tools/development/check-architecture.mjs` 仅是开发期检查，`--details` 查看回归套件归属，不构成产品 Node 依赖。

新增后端计算、状态、协议和实际执行使用 Rust，放入已有领域模块。纯规则只能使用 `module-ownership.json` 的 `rustApps[].allowedDomainDependencies`，保持禁止循环与禁止反向依赖 I/O 层；跨域执行接线须逐文件登记 `integrationModules` 的 `allowedDomains`，真实网络、文件、线程和推理执行须列入 `executionModules`。名称叫 `gateway_*` 不会自动获得跨域权限，不通过宽泛前缀放行。UI 继续使用 C#/XAML/WebView2，启动与构建接线使用 PowerShell；开发工具可使用 Node；安装包的私有 Node 仅用于 MCP 和已核验的 JS Skill，不承担正式后端职责。迁移以行为兼容、单一规则来源和真实调用接线验收，不以行数或文件数量替代工程结果。

## 当前语言与入口

先按下表确定改动归属，再查实际调用方。详细 Rust 操作关系复用 [原生网关调用图](../../../../docs/architecture/native-gateway.md#代码入口与调用关系)，不在技能中另维护一份业务账本或完整文件清单。

| 语言与载荷 | 代码位置 | 维护边界 |
|---|---|---|
| Rust 网关与核心 | `apps/agent-core/src/bin/kynxa-gateway.rs`、`src/gateway.rs` 及六个领域目录 | HTTP/SSE、模型协议与推理、任务/上下文、工具、资源接线、正式存储和检索；源文件的跨域许可由机器清单核验 |
| Rust 资源服务 | `apps/resource-service/src/main.rs` | 实际硬件观测、准入和租约；不由界面或扩展自报可用容量 |
| C#/XAML 桌面 | `apps/desktop/`，启动接线为 `Services/Integration/ModelGatewayService.cs` | WinUI 控件、视图状态、API 客户端与原生网关生命周期；不直接改写正式聊天和索引 |
| C# 共享合同与 ToolHost | `apps/shared/`、`apps/tool-host/Program.cs` 及功能目录 | 两端 DTO 和 Windows 操作；ToolHost 执行已获准动作，Rust 统一持有 MCP 生命周期 |
| WebView JavaScript/HTML/CSS | `apps/desktop/Resources/Transcript/` | 流式 DOM、Markdown/KaTeX/Mermaid 展示、选择与复制；脚本运行于 WebView，不需要 Node 后端 |
| PowerShell | `apps/desktop/Build/`、`tools/development/` 和现有测试启动脚本 | 准备、校验、构建及启动已有载荷；不承接任务循环或正式持久化业务 |
| 开发用 JavaScript/ESM | `tools/development/`、开发预览和测试目录 | 架构守卫、开发工具和隔离夹具；不能把开发依赖加入正式网关运行链路 |
| 扩展兼容运行时与静态资料 | 随包 `runtime/mcp/`，声明与原文位于 `apps/official-tools/` | 固定私有 Node/MCP 依赖供 Rust 管理的扩展使用；第三方 Skill、库和许可证保持原文 |

Rust edition、最低编译器和依赖读取各 crate 的 `Cargo.toml`/`Cargo.lock`；.NET/Windows SDK 和随包目标读取当前 `.csproj`/Build 目标。不把某台机器的工具链路径、下载目录或系统 PATH 写成团队前置条件。用户安装后的运行时由随包资产满足，开发机的编译工具是另一层要求。

| 位置或角色 | 应承担的职责 | 应避免的职责 |
|---|---|---|
| `Views/` | 布局、事件接线、窗口和可见状态 | 将新的复杂业务、协议解析及正式日志写入塞进事件回调 |
| `Controls/` | 可复用视觉、控件交互和生命周期 | 拥有项目/聊天归属或直接保存正式会话 |
| `ViewModels/` | 展示状态、命令、变更通知 | 文件存储和协议实现 |
| 应用流程/业务服务 | 协调创建、发送、重试、切换及状态转换 | 依赖某个具体页面，或同时承包所有功能 |
| API 客户端 | 请求、响应、流读取、错误与取消 | 复制网关的正式持久化逻辑 |
| 模型及共享契约 | 身份、状态、传输数据与纯校验 | 窗口创建、HTTP、磁盘副作用 |
| 网关路由 | 解码与校验输入、调用服务、返回协议结果 | 直接完成整套记忆、文件事务和供应商请求 |
| 网关业务模块 | 会话、记忆、上下文、生成流程 | 混入桌面布局或控件规则 |
| 仓储与适配器 | 文件/索引/迁移和供应商协议映射 | 决定界面选择状态或长期记忆确认策略 |

应用流程是推荐的职责边界，当前未必有独立目录。`ShellPage.*` 的多个 `partial` 文件仍共享页面状态，不能仅凭文件拆分声称已实现完整 MVVM。`ProjectStore` 当前是异步会话 API 客户端，理解其实际职责，不根据旧名称新增第二份本地正式存储。

纯业务服务不引用 Views 或 Controls。`UiLocalization` 等明确属于 UI 的服务可使用界面类型。共享 DTO 不依赖 WinUI；网关不依赖桌面程序集。出现循环依赖先调整职责，不用全局单例和回调链掩盖。

## 结构清晰，接口明确

- 一个模块有一个可解释的主要职责。大文件按行为边界拆分，不设任意行数上限，也不把同一状态散到无关目录。
- 当校验、计算、I/O、展示可独立复用或验证时分开。抽取有意义的模块，不为每行代码创建包装函数。
- 新类型和函数说明其对象与动作，例如 `MemoryRepository`、`LoadConversationAsync`；避免持续扩大的 `Helper`、`Utils`、万能 `Manager`。
- 只暴露调用方需要的 API。接口用于外部 I/O、真实替换点和跨模块契约，不为每个类机械建立同名接口。
- 布局/排序等纯计算放到现有状态或计算模块；尺寸、字号、边界复用现有资源和 [布局入口](../../../../apps/desktop/Layout/ShellLayoutMetrics.cs)。
- 复杂条件和有副作用的分支展开书写，避免嵌套三元表达式、隐含状态变化与无法追踪的长回调。
- 同一规则只保留一个来源。更改规则时查找调用方和兼容路径，避免前端、网关及测试各自维护相近但不同的定义。

## 命名与书写

统一规则由 [代码规范 Skill](../../kynxa-code-standards/SKILL.md) 及其 [命名与双语注释](../../kynxa-code-standards/references/naming-comments.md) 维护，格式入口是 [`.editorconfig`](../../../../.editorconfig)。C#/PowerShell 4 空格、JS/ESM 和 JSON/YAML 2 空格；自有说明性英文注释保留英文并补中文。局部修改不夹带整文件格式化，第三方包字节与许可证保持原文。

命名规范不能代替身份边界：`ConversationId`、`RequestId`、`UserMessageId` 不互换；标题和路径不能替代稳定 ID。错误码、协议字段、模型 ID 与日志事件保持合同；用户可见文案继续走本地化入口。

## 异步、并发与资源

- 除框架事件外返回 `Task`，不要使用 `async void`。UI 线程不调用 `.Wait()`/`.Result` 等阻塞等待。
- 将 C# `CancellationToken` 和 Rust 请求取消信号传到网络、读取、扫描与推理操作；明确原子写入的提交边界，取消后已发生的副作用仍须保存真实回执。
- 从可变 UI 集合取得所需快照后再跨越首个 `await`。回写前检查会话 ID、请求 ID、实例或代次，晚到结果只能影响其所属对象。
- 有意后台运行的任务必须观察异常并有生命周期约束；Rust 异步任务、阻塞线程及 C# Task 均不能成为污染下一聊天的无主作业。
- 所有者负责释放订阅、计时器、指针捕获、流和进程。切换会话、卸载和关闭窗口均应考虑过期操作。
- 明确区分取消、超时、冲突、数据损坏和其他故障；不把 I/O 错误当成空数据或成功吞掉。
- 维持队列和锁的现有顺序。队列可恢复以继续后续操作，但本次失败必须返回调用方；受保护的并行操作全部结束后才释放保护范围。
- 缓存必须有容量和失效条件；流式输出合并刷新，不随每个 token 重建整段聊天或重排侧栏。

## 接口与兼容

- 请求、响应、流事件按方向定义。改字段时同时检查 [共享契约](../../../../apps/shared/Chat/ChatApiContracts.cs)、桌面客户端、网关与模拟上游。
- 当前聊天 HTTP 字段和目录/持久化消息存在不同大小写约定，保持既有合同；不能为命名统一直接改变 JSON 格式。
- 新字段明确旧数据默认值、未知值行为和版本判断。跨版本需要迁移或拒绝时返回可辨认的错误，不静默覆盖未来版本文件。
- 保留 HTTP 状态码与机器错误码，UI 再转换为可读提示。模型服务的认证字段进入对应请求，不进入模型提示词。
- C# 文件被多个 smoke 项目通过 `Compile Include` 链接时，移动或增加依赖同时检查这些项目。
- 使用现有技术和包；增加依赖先确认必要性、发布资源与生命周期，不因参考项目采用某库就照搬其架构。
- 模型调用计量依据真实 HTTP attempt 回执，`requestDispatched` 不等于供应商收费证明；本地参数拒绝或发送前取消不能记为已调用。消息传输 `Status=completed` 不等于任务核验成功，必须保留 `taskCompletion` 的完成、部分、核验失败或执行未知状态。
