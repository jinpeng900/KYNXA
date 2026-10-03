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

更新日期：2026-10-02；已推送代码基线 `3226527`。以下同时记录当前工作区已验证的侧栏修复，该修复不在上述提交中。

* Windows 桌面使用 C# / WinUI 3 / XAML，WebView2 展示可选择复制的 Markdown、代码高亮与 KaTeX 公式。
* 自有 Node.js 网关连接云端或本地 HTTP 模型服务，支持 Chat Completions、Responses、Claude Messages，以及流式、取消和接口实际返回的可见思考。
* 正式聊天按稳定聊天 ID 保存完整 JSONL 事件；切换服务商或模型继续使用同一历史。
* 工作侧栏包含最近、项目和任务。项目旁及任务旁的新增按钮会显示临时“新聊天”；重选同一项目保留输入，切换其他项目丢弃未发送草稿，发送后才正式保存并更新最近顺序。
* 已确认记忆分为聊天、工作、用户三层。同一工作共享确认约定，各聊天原始历史独立；归档、删除、撤销与移动聊天有对应来源和范围规则。
* 用户可以选择 Data 根目录，自动初始化及校验迁移保留稳定 ID、记录、记忆、附件和设置；SQLite 当前是可重建的元数据索引。
* 每个模型连接可选 8K、32K、128K、256K、1M 或自定义上下文预算，须匹配服务端实际能力。当前单次输出最多 2048 tokens，尚未提供独立输出上限配置。

`3226527` 基线网关自动测试 151 项通过。本次侧栏修复另通过 55 项状态检查和 Windows 桌面编译（零警告、零错误）；实际界面检查了两个新增入口、草稿替换、同项目保留输入及切项目清理，正式聊天目录版本未改变。本轮没有重跑上述网关测试；这些证据不代表所有服务商、真实 1M 模型或全部 UI 性能场景已经验收。

已接入模型文件工具、按请求固定的权限与单次审批、stdio MCP、应用技能和 Windows AppContainer 终端。设置可打开“工具与技能”，已启用 MCP 程序是用户信任的外部进程，不属于终端沙箱。完整 Host 编排、崩溃后精确续跑、自动语义记忆和全文/向量检索仍属计划；权限及命令范围见 [基础工具指南](docs/architecture/agent-tools.md)。

使用与开发入口：[模型网关](apps/model-gateway/README.md)、[聊天与工作记忆](docs/architecture/chat-work-memory.md)、[UI 组件](apps/desktop/UI-COMPONENTS.md)、[五人计划](docs/team/README.md)。

## 下一步建议

1. 设置中的记忆管理已实现三范围列表、来源状态、手动新增、编辑、单条删除和版本冲突处理。下一步补独立输出长度配置。上下文取舍提示随后接入，批量清除单独补后端接口与一致性验证。
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

正式架构中，可信的 `kynxa-authority` Rust 进程与较低可信度的 Orchestrator、AI Service 和第三方能力分离。

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
Node.js 模型网关 ───► 云端 API / 本地 HTTP 模型服务
          │
          ▼
Data：目录元信息 + JSONL 正式记录 + 分层确认记忆
      可重建的历史摘录 + SQLite 元数据索引
```

下图是后续运行时设计。Host 语言尚未冻结，团队建议 .NET 10；旧版 Rust Host 属于历史路线。Host 与 Authority 当前均未实现。

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
* **模型网关：** Node.js 内置 HTTP、fetch 与文件接口，目前不依赖 Python 服务。
* **本地模型连接：** llama.cpp、Ollama、LM Studio 等 HTTP 服务；推理进程独立运行。
* **持久化：** 带版本的 JSON 元信息、JSONL 会话事件、分层记忆文件；SQLite 元数据索引。
* **桌面通信：** 与网关通过本机 HTTP / SSE 通信。

后续 Host（建议 .NET 10）、Rust Authority、执行存储和 Named Pipe 仍需单独决策与实现。Python AI Service 是可选设计方向，不是当前运行依赖。

macOS、Linux 和 iOS 暂不属于首版目标范围。

## 仓库结构

```text
KYNXA/
├── apps/
│   ├── desktop/              # Windows WinUI 3 正式前端
│   ├── model-gateway/        # 当前 Node 模型、会话与记忆服务
│   ├── shared/               # 当前 C# 聊天契约
│   ├── desktop-preview/      # 早期视觉与交互预览
│   └── mock-backend/         # 早期模拟服务
├── docs/
│   ├── architecture/        # 已实现的聊天/工作记忆架构
│   ├── team/                # 分工、当前状态及后续计划
│   └── design/              # 更完整的设计参考
└── tests/                   # 桌面、存储、协议与交互 smoke 项目
```

网关单元/集成测试位于 `apps/model-gateway/tests/`。设计中的 `apps/host/`、`crates/` 等目录当前尚未建立。

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

* [模型网关与本地启动](apps/model-gateway/README.md)
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

五人共同开发请从[团队分工与个人任务清单](docs/team/README.md)开始；[队长总览](docs/team/队长总览.md)汇总当前实现、接口边界、六周建议排期和验收要求。队长计入五人，具体姓名和投入时间待团队确认。

随着核心架构和 Public API 稳定，会逐步完善贡献规范。

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
