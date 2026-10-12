# KYNXA

<p align="center">
  <a href="./README.md">English</a> | 中文
</p>

**KYNXA** 是一个面向 Windows 的 local-first 个人 Agent Runtime，重点关注长期任务、模型无关状态、受控能力执行以及自适应运行。

> **当前状态：** 早期开发阶段
> **已实现基线：** 流式聊天、可迁移的会话存储、分层确认记忆
> **下一运行时里程碑（计划）：** `v0.1.0 — CodeRepair Vertical Slice`
> **设计规范版本：** `v1.4.2`

KYNXA 当前仍处于积极开发阶段。完整设计文档中描述的很多能力属于规划架构，并不代表目前已经完成实现。

## 当前实现

更新日期：2026-10-12。原生后端迁移位于当前工作区，构建与验收记录和旧版结果分开；完整运行时设计中尚未落地的能力仍属计划。

* Windows 桌面使用 C# / WinUI 3 / XAML，WebView2 展示可选择复制的 Markdown、代码高亮与 KaTeX 公式。
* Rust 原生网关连接云端或本地 HTTP 模型服务，支持 Chat Completions、Responses、Claude Messages、Ollama，以及流式和取消。核心不依赖 Node，不恢复 JS 网关；安装包独立随附固定私有 Node 和已安装的内置 MCP 依赖。WebView 渲染脚本保留为前端资源。
* Rust 管理 HTTP/SSE、模型与 MCP I/O、任务恢复、上下文压缩、工具、检索及原始 JSONL/SQLite 事务；独立 Rust 资源服务负责真实硬件准入。桌面 `dotnet run` 命令保留，`node server.mjs` 淘汰，详见 [原生网关与验收边界](docs/architecture/native-gateway.md)。
* 正式聊天按稳定聊天 ID 保存完整 JSONL 事件；切换服务商或模型继续使用同一历史。
* 工作侧栏包含最近、项目和任务。项目旁及任务旁的新增按钮会显示临时“新聊天”；重选同一项目保留输入，切换其他项目丢弃未发送草稿，发送后才正式保存并更新最近顺序。
* 已确认记忆分为聊天、工作、用户三层。同一工作共享确认约定，各聊天原始历史独立；归档、删除、撤销与移动聊天有对应来源和范围规则。
* 用户可以选择 Data 根目录，自动初始化及校验迁移保留稳定 ID、记录、记忆、附件和设置；独立 SQLite 索引覆盖侧栏元数据和有范围隔离的本地检索。
* 设置已增加全局/工作检索与网页搜索：显式资料导入、挂载工作资料自动准备、词法/向量检索及后台任务。离线多语言嵌入模型随包，不要求 Python 或数据库服务；简单问候跳过检索和工具准备。实际范围、资源限制与后续工作见 [当前检索架构](docs/architecture/retrieval.md)。
* 上下文由运行时依据已核验模型能力和实际本地服务观察决定；旧上下文配置仍兼容保存，不能授权模型容量。输出默认上限 256K（262144），提供 4K、8K、16K、32K、64K、128K、256K 和自定义，已知供应商上限与实际输入空间可降低它。未知 API 部署的所有限制无法预先得知；已识别的 HTTP 400 上下文拒绝在当前请求内作有界恢复。
* 同一聊天保存全部消息，已验证超过 200 条的重启和回源；输入超预算时选取近期完整问答、相关旧约束与代码摘录。模型可分页查回当前聊天原文，工具循环压缩已保存结果的请求预览，不删除正式记录。

设置包含聊天、工作和全局记忆管理。原生回归使用隔离数据和本机模拟上游，当前命令见 [核心说明](apps/agent-core/README.md)。历史检查不代表新网关、所有服务商或真实 1M 模型已验收。

已接入模型文件工具、按请求固定的权限与单次审批、stdio / Streamable HTTP MCP、应用技能和 Windows AppContainer 终端。设置中的“工具与技能”支持官方服务预设、环境变量认证引用、技能导入与启停；已启用 MCP 程序是用户信任的外部进程，不属于终端沙箱。完整 Host 编排、崩溃后精确续跑和自动语义记忆仍属计划；有范围隔离的本地全文/向量检索已落地，权限及命令范围见 [基础工具指南](docs/architecture/agent-tools.md)。

没有关联文件夹的聊天也有独立、可持久化的工具目录和 AppContainer 终端快照。原生工具支持窗口截屏、发现/打开 GUI 软件、读取可访问文字和有目标校验的输入；运行依赖随包发布。本机操作仍经过权限与审批，截图可按所选模型协议投影为图像输入，是否理解图像取决于模型实际能力。浏览器 DOM 由原生 CDP 和已配置的兼容 MCP 提供。

右侧栏使用可横向滚动的截图标签，宽度随侧栏调整；用户正在查看某张图片时，新截图进入后台标签。标签可以关闭和重新打开，图片支持原像素全屏查看。隐藏侧栏暂停图片读取，切换聊天清除旧图，重新打开从正式记录恢复。终端不在右侧渲染；本机 CMD/PowerShell 的输出与退出状态继续保存于正式工具回执，明确要求时仍可打开独立可见终端。

MCP 发现目录与模型声明预算分离，支持单工具禁用、搜索和按需加载。带类型与结构化的完整结果按聊天保存，通过引用和分页预览读取；私有 MCP 元信息不进入模型或详情投影。第三方参数与审批理由分开，技能头部支持受限 YAML 1.2，取消先保存已返回的执行结果。

技能资源按技能目录解析，保留标准校验和依赖诊断。Markdown 技能、远程 MCP 和原生可执行 MCP 继续可用；固定内置 Node MCP 使用私有运行时及已安装入口，不要求系统 PATH，启动不调用 npm/npx 下载；JS 技能通过已有沙箱使用核验过的私有 Node。账号、API Key、数据库配置和未随附的第三方依赖仍明确报告。原生 CDP 提供浏览器自动化；保留用户明确关闭选择，读取设置不启动服务，启用不等于就绪。

设置保留独立 Data 与用户工具位置。扩展迁移先校验复制再切换指针，保留原文件；更改 Data 不移动单独配置的扩展。官方静态工具包源码在 `apps/official-tools/`，随包位于 `official-tools/`，不含可执行后端 JS。公共网页读取、文件与原生浏览器/终端继续经过已有授权边界，用户扩展独立保存。

使用与开发入口：[原生网关](docs/architecture/native-gateway.md)、[核心说明](apps/agent-core/README.md)、[聊天与工作记忆](docs/architecture/chat-work-memory.md)、[UI 组件](apps/desktop/UI-COMPONENTS.md)、[五人分工](docs/team/README.md)。

## 五人代码边界

A 主责 Rust 网关组合、`orchestration/` 与 `resources/`；C 主责 `models/`；D 主责 `tools/`、ToolHost 与 `apps/official-tools/`；E 主责 `storage/`、`retrieval/`。原生模块位于 `apps/agent-core/src/`。B 唯一负责全部 ShellPage partial、WinUI 与聊天展示。

桌面服务分为 `Integration/Presentation/Models/Tools/Data`，共享契约分为 `Chat/Tools/Memory`，ToolHost 分为 `Desktop/Terminal/Sandbox/Native`。生产 CLI 为 `kynxa-gateway.exe`，桌面启动命令保留；目录职责不代表完整 MVVM 或跨程序集隔离。

纯领域规则继续检查依赖与循环；明确的 I/O 组合模块逐文件列出允许调用的领域，复用正式账本与事务。第三方技能原文和许可证保留，实际边界见 [团队边界](docs/architecture/team-boundaries.md)，交接见 [五人分工](docs/team/README.md)。本轮验证另记，不沿用历史结果冒充验收。

## 下一步建议

1. 记忆管理、独立输出配置与可回源的上下文摘录已实现。下一步展示实际上下文取舍，再按评测结果补可验证的语义摘要；批量清除需要独立后端接口与一致性验证。
2. 将聊天列表与正文加载拆开，按需加载当前聊天，量测长历史的切换、滚动与复制。
3. 在已接入的工作文件与工具闭环上增加补丁预览、多文件事务和持久执行检查点。正式聊天存储沿用现有单一来源，替换前须有明确迁移方案。
4. 明确服务生命周期与失败处理后，补本地服务发现/启停及模型下载管理。

除已落地的记忆管理外，以上为后续建议；负责人和验收标准见 [团队计划](docs/team/README.md)，记忆界面的接口与实施切分见 [下一轮实施](docs/architecture/chat-work-memory.md#下一轮实施切分)。

## 项目愿景

大多数 AI 助手以 Conversation 为中心。

KYNXA 希望以 **Work** 为中心。

一个 Work 代表一个可长期持续存在的任务。它可能跨越多次模型调用、多个工具、多个应用、程序崩溃、系统重启甚至模型切换。

KYNXA 的目标是构建一个能够：

* 理解并持续执行长期目标；
* 调用本地工具和应用完成真实任务；
* 不依赖单一模型保存任务状态；
* 在中断、崩溃后恢复工作；
* 对结果进行验证，而不是仅仅声称“已经完成”；
* 在执行敏感操作前请求明确授权；
* 根据任务和硬件条件协调本地模型与云端模型；
* 让普通用户无需理解复杂 Agent 基础设施也能直接使用。

## 核心原则

以下原则描述目标运行时。当前聊天/工作确认记忆尚不包含持久执行状态、任务图或执行检查点。

### Durable Work

长期任务是系统中的一等实体，而不是一次性的聊天 Session。

一个 Work 可以包含：

* Goal；
* CognitiveState；
* Task Graph；
* Artifact；
* Evidence；
* 执行历史；
* Approval；
* Checkpoint；
* Side-effect State。

### Model-Independent CognitiveState

任务连续性不应依赖某一个模型的 Context Window、隐藏状态或者 KV Cache。

KYNXA 使用结构化 CognitiveState 保存任务认知状态，并允许在需要时向不同能力模型重新投影上下文。

### Governed Capabilities

模型不能直接拥有宿主机高权限。

所有敏感操作必须经过 KYNXA 的可信 Authority Boundary。

设计规范提出独立可信 Authority；当前实际实现由网关策略与审批约束执行，并分派到已有 C# ToolHost，尚未实现独立 Rust Authority。

### Verifiable Execution

KYNXA 优先使用确定性方法验证任务结果。

例如：

* 编译结果；
* 单元测试；
* Exit Code；
* Schema Validation；
* 文件存在性；
* Artifact Hash。

只有确定性验证无法完成时，才升级到 LLM Verifier。

### Adaptive Execution

KYNXA 的长期目标是根据任务状态和本地硬件情况动态协调：

* Model；
* Context；
* Skill；
* Tool；
* Inference Profile；
* Verification Strategy；
* 本地硬件资源。

研究目标是在消费级硬件约束下提高 Agent 的实际任务完成能力。

## 总体架构

当前实际运行结构：

```text
WinUI 桌面 + WebView2
          │ 本机 HTTP / SSE
          ▼
Rust 模型网关 ─────► 云端 API / 本地 HTTP 模型服务
          ├────────► C# ToolHost：桌面 / 终端 / 沙箱 / 原生执行
          │
          ▼
Data：目录元信息 + JSONL 正式记录 + 分层确认记忆
      可重建的历史摘录 + SQLite 元数据索引
```

下图保留为历史后续运行时设计，与当前 Rust 网关及 C# ToolHost 分开理解；它不代表实际运行中存在独立 Authority。

```text
                 KYNXA Desktop
              C# + WinUI 3 + XAML
                       │
                       │ IPC
                       ▼
               kynxa-host.exe
            Host（语言待决策）
                       │
          ┌────────────┼────────────┐
          │            │            │
          ▼            ▼            ▼
   AI Service      Sandbox      Browser/Tools
  较低可信度        隔离执行        隔离执行
          │            │            │
          └────────────┼────────────┘
                       │
              Capability Request
                       │
                       ▼
              kynxa-authority.exe
                Trusted Rust Core
                       │
       Policy · Approval · Credential
           Audit · Privileged Execution
                       │
                       ▼
                    Windows
```

桌面 GUI 与 Runtime 强制分离。

后续执行边界要求 Desktop Client 不应直接：

* 操作高权限文件；
* 执行宿主机 Shell；
* 读取 Credential；
* 修改 Task Graph；
* 绕过授权；
* 执行敏感 Capability。

## 当前开发目标

第一个工程里程碑：

### `v0.1.0 — CodeRepair Vertical Slice`

目标工作流：

```text
创建 Work
    ↓
分析真实代码仓库
    ↓
建立 Task Graph
    ↓
读取并修改代码
    ↓
编译 / 测试
    ↓
发现失败
    ↓
重新修复
    ↓
Verifier 验证
    ↓
Checkpoint
    ↓
强制关闭 / 重启 Host
    ↓
Resume Work
    ↓
完成经过验证的 Artifact
```

这一阶段的目的不是实现全部 KYNXA 功能，而是首先证明核心 Runtime 架构能够真正运行。

## 规划中的主要模块

当前完整设计包含：

* Work 与 Conversation
* CognitiveState
* Durable Task Graph
* Adaptive Harness
* 本地与云端模型 Runtime
* File / Code / Shell
* Browser / Computer
* Knowledge / Memory
* Skill / Tool / MCP 生态
* Trusted Authority Core
* Approval Center
* Checkpoint / Replay / Recovery
* Cache 与 Observability
* Routine 与自动化
* Android Resident
* Personalization 与 Shadow Mode
* Agent Wallet 与 Commerce
* Network / Web Search / VPN / Proxy

这些模块当前处于不同的设计和实现阶段。

## 技术方向

首版平台范围：

* **桌面端：Windows**
* **移动端：Android**

当前技术栈：

* **桌面 UI：** C# + WinUI 3 + XAML；WebView2 / KaTeX 展示会话。
* **模型网关：** Rust HTTP/SSE、协议解码、MCP/CDP、ONNX 推理与文档解析；私有 Node 仅供扩展及已核验的沙箱脚本使用，核心不依赖它。不携带 Python 运行时。
* **本地模型连接：** llama.cpp、Ollama、LM Studio 等 HTTP 服务；推理进程独立运行。
* **持久化：** 带版本的 JSON 元信息、JSONL 会话事件、分层记忆文件；SQLite 元数据索引。
* **桌面通信：** 与网关通过本机 HTTP / SSE 通信。

C# ToolHost 承担 Windows 原生执行；独立 Host/Authority 与 Named Pipe 保留为设计参考，当前实现使用 WinUI、Rust 与 C#。

macOS、Linux 和 iOS 暂不属于首版目标范围。

## 仓库结构

```text
KYNXA/
├── apps/
│   ├── desktop/              # Windows WinUI 3 正式前端
│   ├── agent-core/           # Rust HTTP/SSE、Agent、模型、工具、检索与存储
│   ├── resource-service/     # 原生硬件准入与资源租约
│   ├── official-tools/       # 静态技能、配置预设与许可证
│   ├── shared/               # 当前 C# 聊天、记忆与工具契约
│   ├── tool-host/            # Windows 桌面、终端、沙箱与技能原生执行
│   ├── desktop-preview/      # 早期视觉与交互预览
│   └── mock-backend/         # 早期模拟服务
├── docs/
│   ├── architecture/        # 已实现的聊天/工作记忆架构
│   ├── team/                # 分工、当前状态及后续计划
│   └── design/              # 更完整的设计参考
└── tests/                   # 桌面、存储、协议与交互 smoke 项目
```

原生单元/集成测试位于 `apps/agent-core/src/`、`apps/agent-core/tests/`；随包检查在 `tests/native-runtime-smoke/`。设计中的 `apps/host/`、`crates/` 等目录当前尚未建立。

## 安全模型

KYNXA 默认假设下列内容可能是不可信的：

* LLM 输出；
* 网页内容；
* 第三方 Skill；
* Parser；
* AI Service；
* 外部 GitHub 项目；
* Browser Content。

因此安全系统不能依赖自然语言模型“主动遵守规则”。

计划中的 Trusted Authority Layer 将负责：

* Policy Evaluation；
* Capability Authorization；
* Scope Validation；
* Approval Verification；
* AuthorizationGrant 生命周期；
* Credential Access；
* Security Audit；
* 高权限执行。

VPN 或代理只改变网络流量的路径。

它们**不会自动授予 Network Capability**。

## 网络与 Web Search

KYNXA 明确分离：

```text
Model Provider
≠
Search Provider
≠
Network Route
≠
Authority Policy
```

后续设计允许本地模型通过受控的 Web Search 能力获取网络信息；当前尚未实现搜索工具和能力级网络控制。

计划支持：

* Windows 系统 VPN / TUN；
* 规则模式代理；
* Windows System Proxy；
* HTTP / HTTPS Proxy；
* SOCKS5；
* Direct Mode；
* Capability 级网络 Policy。

## 文档体系

KYNXA 维护两个层级的文档。

### Developer Documentation

面向用户和 Contributor 的精简 Markdown 文档。

* [原生网关与本地启动](docs/architecture/native-gateway.md)
* [聊天/工作记忆与 Data 目录](docs/architecture/chat-work-memory.md)
* [UI 组件](apps/desktop/UI-COMPONENTS.md)
* [五人分工与下一步](docs/team/README.md)
* [队长总览](docs/team/队长总览.md)
* [R3 设计参考](docs/design/r3-refined/README.md)

### Full Technical Design Specification

完整设计规范记录架构、安全、Runtime、网络、产品、研究和实现决策。

设计规范版本与软件版本互相独立。

例如：

```text
Design Specification: v1.4.2
Software Release:     v0.1.0
```

## 工程原则

KYNXA 开发过程中遵循：

* 不瞎猜接口；
* 需求不清时先确认；
* 不臆想业务行为；
* 优先复用已有接口；
* 主动验证和测试；
* 遵守架构和工程规范；
* 如实表达不确定性；
* 避免盲改和无必要的大规模重构。

项目会大量使用 AI 辅助开发，但架构决策、接口契约、测试和最终验证仍需要由工程流程负责。

## Roadmap

### v0.1

CodeRepair Vertical Slice

* Work
* Task Graph
* CognitiveState
* File / Code / Shell
* Trusted Authority Core
* Verifier
* Checkpoint / Resume

### v0.2

Knowledge 与 Browser 基础能力

### v0.3

Skill / Tool / MCP 生态

### v0.4

Automation 与 Android 基础能力

### v0.5

Feature-complete Alpha

### v1.0

Stable Public Release

Roadmap 会根据实际开发和 Benchmark 结果调整。

## Contributing

KYNXA 当前仍处于早期开发阶段。

五人共同开发请从[团队分工与个人任务清单](docs/team/README.md)开始；[队长总览](docs/team/队长总览.md)汇总实际模块、接口边界和验收要求。队长计入五人。[GitHub 协作](docs/team/github-collaboration.md)提供成员邀请命令、CODEOWNERS 生成和主分支保护设置；目录主责是审查归属，不是文件夹级 Git 权限。

随着核心架构和 Public API 稳定，会逐步完善贡献规范。

仓库代码开发同时使用 [开发 Skill](.agents/skills/kynxa-development/SKILL.md) 与 [代码书写 Skill](.agents/skills/kynxa-code-standards/SKILL.md)，由 `AGENTS.md` 自动引导。后者统一语义命名、中英文说明注释、各语言格式及兼容例外；公开协议字段和第三方原始内容保持兼容。这是开发代理的规则，区别于应用内模型使用的技能。

未来欢迎：

* Bug Report；
* Architecture Discussion；
* Implementation Proposal；
* Test；
* Documentation；
* Security Review；
* Benchmark。

## License

开源许可证将在正式公开发布前最终冻结。

## Disclaimer

KYNXA 当前属于实验性软件。

Agent 系统可能与文件、进程、浏览器、外部服务以及其他敏感资源进行交互。

正式发布版本必须建立明确的 Capability Boundary，并且绝不能将模型输出本身视为可信授权。
