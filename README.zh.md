# KYNXA

<p align="center">
  <a href="./README.md">English</a> | 中文
</p>

**KYNXA** 是一个面向 Windows 的 local-first 个人 Agent Runtime，重点关注长期任务、模型无关状态、受控能力执行以及自适应运行。

> **当前状态：** 早期开发阶段
> **当前里程碑：** `v0.1.0 — CodeRepair Vertical Slice`
> **设计规范版本：** `v1.4.2`

KYNXA 当前仍处于积极开发阶段。完整设计文档中描述的很多能力属于规划架构，并不代表目前已经完成实现。

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

```text
                 KYNXA Desktop
              C# + WinUI 3 + XAML
                       │
                       │ IPC
                       ▼
               kynxa-host.exe
               Rust Orchestrator
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

Desktop Client 不应直接：

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

主要技术栈：

* **桌面 UI：** C# + WinUI 3 + XAML
* **核心 Runtime：** Rust
* **Trusted Authority Core：** Rust
* **AI Service：** Python
* **本地模型 Runtime：** llama.cpp 兼容 Runtime 及其他本地 Backend
* **持久化：** SQLite
* **本地 IPC：** Windows Named Pipe

macOS、Linux 和 iOS 暂不属于首版目标范围。

## 仓库结构

```text
KYNXA/
├── apps/
│   └── desktop/
├── crates/
│   ├── kynxa-core/
│   ├── kynxa-protocol/
│   ├── kynxa-host/
│   ├── kynxa-authority/
│   └── kynxa-storage/
├── services/
│   └── ai-service/
├── docs/
│   ├── en/
│   ├── zh-CN/
│   └── adr/
├── tests/
│   ├── integration/
│   ├── security/
│   └── benchmarks/
└── tools/
```

早期开发阶段仓库结构仍可能根据实现情况调整。

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

Trusted Authority Layer 负责：

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

本地 DeepSeek、Qwen、Llama 等模型可以通过 KYNXA Web Search 获取网络信息，而模型本身不直接拥有任意网络权限。

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
