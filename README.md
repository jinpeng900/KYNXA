# KYNXA

[简体中文](./README.zh-CN.md)

**KYNXA** is a local-first personal agent runtime for Windows, designed for durable tasks, model-independent state, governed capabilities, and adaptive execution.

> **Status:** Early Development
> **Current Milestone:** `v0.1.0 — CodeRepair Vertical Slice`
> **Design Specification:** `v1.4.2`

KYNXA is currently under active development. Many features described in the design documents are planned architecture and are not yet implemented.

## Vision

Most AI assistants are centered around conversations.

KYNXA is designed around **Work**.

A Work represents a persistent task that may span multiple model calls, tools, applications, failures, restarts, and even model changes.

The goal is to build a personal agent that can:

* understand long-running goals;
* execute tasks across local tools and applications;
* preserve task state independently of a specific model;
* recover after interruptions or crashes;
* verify results instead of merely claiming completion;
* request explicit authorization before sensitive actions;
* use local and cloud models according to task and hardware conditions;
* remain usable without requiring users to understand agent infrastructure.

## Core Principles

### Durable Work

Tasks are persistent first-class entities rather than temporary chat sessions.

A Work may contain:

* goals;
* CognitiveState;
* Task Graph;
* artifacts;
* evidence;
* execution history;
* approvals;
* checkpoints;
* side-effect state.

### Model-Independent CognitiveState

Task continuity should not depend on one model's context window, hidden state, or KV cache.

KYNXA maintains a structured CognitiveState that can be reconstructed and projected to different models when necessary.

### Governed Capabilities

Models do not directly own host privileges.

Sensitive operations must pass through KYNXA's trusted authority boundary.

The production architecture separates the trusted `kynxa-authority` process from the less-trusted orchestration and intelligence plane.

### Verifiable Execution

KYNXA prefers deterministic verification whenever possible.

Examples include:

* compiler results;
* unit tests;
* exit codes;
* schema validation;
* file existence checks;
* artifact hashes.

LLM-based verification is used only when deterministic verification is insufficient.

### Adaptive Execution

KYNXA is designed to dynamically coordinate:

* models;
* context;
* Skills;
* tools;
* inference profiles;
* verification strategies;
* local hardware resources.

The long-term research goal is to maximize useful agent capability under consumer-grade hardware and runtime constraints.

## Architecture

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
 Less-Trusted     Isolated       Isolated
          │            │            │
          └────────────┼────────────┘
                       │
              Capability Request
                       │
                       ▼
              kynxa-authority.exe
                 Trusted Rust Core
                       │
     Policy · Approval · Credential · Audit
             Privileged Execution
                       │
                       ▼
                    Windows
```

The desktop client is intentionally separated from the runtime.

The UI must not directly:

* access privileged files;
* execute host shell commands;
* access credentials;
* modify Task Graph state;
* bypass authorization;
* invoke privileged capabilities.

## Current Milestone

The first engineering milestone is:

### `v0.1.0 — CodeRepair Vertical Slice`

Target workflow:

```text
Create Work
    ↓
Analyze repository
    ↓
Build Task Graph
    ↓
Read and modify code
    ↓
Compile / test
    ↓
Observe failure
    ↓
Repair
    ↓
Verify
    ↓
Checkpoint
    ↓
Kill / restart host
    ↓
Resume Work
    ↓
Complete verified artifact
```

The purpose of this milestone is to validate the core runtime architecture before expanding into broader capabilities.

## Planned Components

The design currently includes the following major subsystems:

* Work and Conversation
* CognitiveState
* Durable Task Graph
* Adaptive Harness
* Local and cloud model runtime
* File / Code / Shell capabilities
* Browser and Computer capabilities
* Knowledge and Memory
* Skill / Tool / MCP ecosystem
* Trusted Authority Core
* Approval Center
* Checkpoint / Replay / Recovery
* Cache and observability
* Routine and automation
* Android Resident
* Personalization and Shadow Mode
* Agent Wallet and commerce architecture
* Network, Web Search, VPN and proxy abstraction

These components are at different stages of design and implementation.

## Technology Direction

Initial platform scope:

* **Desktop:** Windows
* **Mobile:** Android

Primary implementation technologies:

* **Desktop UI:** C# + WinUI 3 + XAML
* **Core Runtime:** Rust
* **Trusted Authority Core:** Rust
* **AI Services:** Python
* **Local Model Runtime:** llama.cpp-compatible runtimes and other supported local backends
* **Persistence:** SQLite
* **Local IPC:** Windows Named Pipes

macOS, Linux, and iOS are not part of the initial product scope.

## Repository Structure

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

The structure may evolve during early development.

## Security Model

KYNXA assumes that the following components may be untrusted or compromised:

* LLM output;
* web content;
* third-party Skills;
* parsers;
* AI services;
* external repositories;
* browser content.

Security decisions must therefore not rely on natural-language compliance.

The trusted authority layer is responsible for:

* policy evaluation;
* capability authorization;
* scope validation;
* approval verification;
* authorization grant lifecycle;
* credential access;
* security audit;
* privileged execution.

A VPN or proxy changes the network route. It does **not** grant network capability.

## Web and Network Architecture

KYNXA separates:

```text
Model Provider
≠
Search Provider
≠
Network Route
≠
Authority Policy
```

A local model such as DeepSeek, Qwen, Llama, or another supported model may use KYNXA Web Search without directly owning network access.

KYNXA is designed to support:

* system VPN/TUN routing;
* rule-based proxy routing;
* Windows system proxy;
* HTTP/HTTPS proxy;
* SOCKS5 proxy;
* direct mode;
* per-capability network policy.

## Documentation

The project maintains two levels of documentation:

### Developer Documentation

Concise Markdown documentation for contributors and users.

### Full Technical Design Specification

The full design specification contains detailed architecture, security, runtime, networking, product, research, and implementation decisions.

The design specification version is independent from the software release version.

For example:

```text
Design Specification: v1.4.2
Software Release:     v0.1.0
```

## Development Philosophy

KYNXA follows several engineering principles:

* do not guess interfaces;
* clarify uncertain requirements before implementation;
* avoid inventing business behavior;
* reuse existing interfaces when possible;
* validate and test proactively;
* respect architecture and contracts;
* state uncertainty explicitly;
* avoid blind refactoring.

AI-assisted development is used extensively, but architectural decisions, interface contracts, tests, and final validation remain part of the engineering process.

## Roadmap

### v0.1

CodeRepair Vertical Slice

* Work
* Task Graph
* CognitiveState
* File / Code / Shell
* Trusted Authority Core
* Verifier
* Checkpoint and Resume

### v0.2

Knowledge and Browser foundations

### v0.3

Skill / Tool / MCP ecosystem

### v0.4

Automation and Android foundations

### v0.5

Feature-complete Alpha

### v1.0

Stable public release

The roadmap may change as implementation and benchmark results reveal better architectural choices.

## Contributing

KYNXA is currently in early development.

Contribution guidelines will be expanded as the architecture and public APIs stabilize.

Bug reports, architecture discussions, implementation proposals, tests, documentation improvements, and security reviews will be welcome.

## License

License information will be finalized before the public release.

## Disclaimer

KYNXA is experimental software.

Agent systems may interact with files, processes, browsers, external services, and other sensitive resources. Production releases must enforce explicit capability boundaries and should never treat model output as trusted authority.
