# KYNXA

<p align="center">
  English | <a href="./README.zh.md">中文</a>
</p>

**KYNXA** is a local-first personal agent runtime for Windows, designed for durable tasks, model-independent state, governed capabilities, and adaptive execution.

> **Status:** Early Development
> **Implemented Baseline:** streamed chat, portable conversation storage, scoped confirmed memory
> **Next Runtime Milestone (planned):** `v0.1.0 — CodeRepair Vertical Slice`
> **Design Specification:** `v1.4.2`

KYNXA is currently under active development. Many features described in the design documents are planned architecture and are not yet implemented.

## Current Implementation

Updated 2026-10-05. This section describes implemented behavior; the larger runtime design remains a plan.

* Windows desktop: C# / WinUI 3 / XAML, with WebView2 for selectable Markdown, code highlighting and KaTeX math.
* A bundled Node.js gateway connects cloud and local HTTP model services using Chat Completions, Responses or Claude Messages. Streaming, cancellation and visible reasoning are supported.
* Complete conversations use stable chat IDs and append-only JSONL events. Switching providers or models retains the same history.
* The work sidebar has Recent, Projects and Tasks. Both project and task add buttons show a temporary new chat. Reselecting its project preserves input; switching projects discards an unsent draft. Sending commits the chat and updates recent activity.
* Confirmed memory has chat, project/work and user scopes. Sibling chats share confirmed work memory while their original histories remain separate. Archive, delete, undo and moving chats preserve the intended scope rules.
* A configurable Data root contains records, memory, attachments and settings. Initialization and verified migration preserve stable IDs; SQLite is currently a rebuildable metadata index.
* Context and output limits are independent connection settings. Output defaults to a 256K (262144) ceiling, with 4K/8K/16K/32K/64K/128K/256K and custom choices; context offers 8K/32K/128K/256K/1M and custom budgets. Both must match the model service. Small context windows reduce the effective output reserve.
* Chats retain all original messages; restart and retrieval are tested beyond 200 messages. Bounded requests preserve recent complete turns and relevant older constraints/code excerpts. Models can page through the current chat's original messages; tool loops compact saved-result previews without deleting records.

Settings includes memory management for chat, work and user scopes. Gateway regressions use temporary data and simulated upstreams; desktop tests cover transport, actual DOM and isolated native windows. Earlier validation records are in the [gateway guide](apps/model-gateway/README.md#当前验证与后续接口). They do not certify every provider or a real 1M model.

Model-controlled file tools, per-request approvals, stdio / Streamable HTTP MCP, application skills and a Windows AppContainer terminal are implemented. Tools and skills settings support official service presets, environment-variable authentication references, skill import and enable switches; enabled MCP programs are trusted external processes, separate from the terminal sandbox. Full Host orchestration, crash-resumable checkpoints, automatic semantic memory and full-text/vector search remain planned. See the [agent tools guide](docs/architecture/agent-tools.md) for permissions and supported commands.

Folderless chats have persistent isolated work directories. Bundled native tools can launch GUI applications, read accessible window text, capture target windows and perform approved targeted input without requiring Python. A separate host CMD/PowerShell tool preserves actual output and execution receipts. The right sidebar uses horizontally scrollable screenshot tabs and original-pixel fullscreen viewing; new screenshots do not replace an already selected tab. Terminal output is kept in execution receipts, and an explicitly requested visible console opens separately.

Large MCP catalogs are discovered separately from the bounded model tool declarations. Individual tools can be disabled or loaded on demand. Typed and structured results are saved with chat-scoped references and paged previews; private MCP metadata stays out of model and viewer projections. Approval reasons are separate from third-party arguments, skill headers use bounded YAML 1.2 parsing, and cancellation records returned execution outcomes before stopping.

Skill resources resolve from their package root, with standard validation, dependency diagnostics and read-only package snapshots for isolated Node scripts. Official Playwright and GitHub MCP presets and an Apache-2.0 communication-writing skill are reused without adding duplicate filesystem or memory services. Official MCP presets are enabled by default; existing explicit user disables are preserved and repeated additions reuse existing configurations. Reading settings does not start services. Missing start commands, referenced credentials or required configuration paths leave a service not ready rather than available. Python/Bash skill scripts and interactive browser OAuth login are not supported; unverified runtime dependencies block execution.

Settings provide separate Data and User tools storage rows. MCP configuration, imported skills and managed npm/browser caches can move to an independent folder through a verified copy and atomic pointer switch. Old files remain available; changing Data keeps an explicitly configured extension folder in place. The read-only official package remains bundled under `model-gateway/official-tools/`, with 35 core tool definitions, seven skills and eleven enabled-by-default MCP presets. Users can disable or customize these services; enabling a preset does not mean it is connected or its dependencies are installed. Built-in public webpage reading and paged file reads need no Python; desktop, browser and terminal workflows use the existing execution boundaries. User extensions and overrides are stored separately.

Start with [code organization and responsibilities](docs/architecture/code-organization.md), the [gateway guide](apps/model-gateway/README.md), [chat/work memory architecture](docs/architecture/chat-work-memory.md), [UI component guide](apps/desktop/UI-COMPONENTS.md) and [five-person plan](docs/team/README.md).

## Five-Person Code Boundaries

As of 2026-10-05, the committed baseline is `5637dc3`; this reorganization is in the workspace. A owns gateway `orchestration/` and coordinates `platform/`; C owns `models/`; D owns `tools/` and `official-tools/`; E owns `data/`. B owns all ShellPage partials, WinUI and transcript presentation.

Desktop services use `Integration/Presentation/Models/Tools/Data`, and shared contracts use `Chat/Tools/Memory`. ToolHost uses `Desktop/Terminal/Sandbox/Native`. The root gateway CLI entries remain stable. These are directory boundaries within the existing applications, not a completed MVVM or assembly split.

Data and Platform do not depend on higher layers. Existing model-history reads the public result preview from Data, and Tools uses pure model helpers; the domains are not entirely independent. The project-owned official Tools catalog import changed; third-party skill text and licenses were preserved. See [actual dependencies and primary source references](docs/architecture/team-boundaries.md) and [developer handoff](docs/team/README.md). Current reorganization validation is recorded separately from earlier results.

## Next Steps

1. Memory management, independent output configuration and retrievable context excerpts are implemented. Expose context decisions next, then evaluate verifiable semantic summaries. Bulk clearing still needs a separate backend contract and consistency checks.
2. Separate conversation lists from message loading; load the selected chat on demand and measure long-history scrolling, copying and switching.
3. Extend the implemented workspace/tool loop with patch previews, explicit multi-file transactions and durable execution checkpoints. Keep the existing conversation store as the single authority until an explicit migration replaces it.
4. Add local-service discovery/startup and model download management after the corresponding lifecycle and failure handling are defined.

Memory management is implemented; the remaining items are proposals. Owners and acceptance criteria are in the [team plan](docs/team/README.md); the [memory implementation breakdown](docs/architecture/chat-work-memory.md#下一轮实施切分) names the existing interfaces and next deliverables.

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

These principles describe the target runtime. Current confirmed chat/work memory does not yet provide durable execution state, task graphs or checkpoints.

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

The design specification proposes a separate trusted authority process. The running implementation applies gateway policy and approvals before dispatching to the existing C# ToolHost; a separate Rust Authority is not implemented.

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

The running implementation is:

```text
WinUI Desktop + WebView2
          │ local HTTP / SSE
          ▼
Node.js model gateway ───► cloud API / local HTTP model server
          ├─────────────► C# ToolHost: desktop / terminal / sandbox / native
          │
          ▼
Data: catalog + JSONL records + scoped memory
      derived context excerpts + SQLite metadata index
```

The following diagram is a historical future-runtime design, separate from the current Node.js gateway and implemented C# ToolHost. It does not prescribe a new Host, language migration or independent Authority for the current five-person work.

```text
                 KYNXA Desktop
              C# + WinUI 3 + XAML
                       │
                       │ IPC
                       ▼
               kynxa-host.exe
           Host (language pending decision)
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

The planned execution boundary requires that the UI must not directly:

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

Current technologies:

* **Desktop UI:** C# + WinUI 3 + XAML; WebView2 / KaTeX for transcripts.
* **Model gateway:** Node.js built-in HTTP, fetch and file APIs; no Python service is required.
* **Local model connection:** HTTP services such as llama.cpp, Ollama and LM Studio; model inference remains a separate process.
* **Persistence:** versioned JSON metadata, JSONL conversation events and scoped memory files; SQLite metadata index.
* **Desktop communication:** local HTTP / SSE to the gateway.

The C# ToolHost already provides native execution. A separate Host/Authority, durable execution storage and Named Pipes remain design references requiring separate decisions; the current team works on the existing WinUI/Node.js/C# chain. Python is not a current dependency.

macOS, Linux, and iOS are not part of the initial product scope.

## Repository Structure

```text
KYNXA/
├── apps/
│   ├── desktop/              # Production Windows WinUI 3 frontend
│   ├── model-gateway/        # Current Node.js model, conversation and memory service
│   ├── shared/               # Current C# chat, memory and tool contracts
│   ├── tool-host/            # Native Windows desktop, terminal, sandbox and skill execution
│   ├── desktop-preview/      # Earlier visual and interaction preview
│   └── mock-backend/         # Earlier mock service
├── docs/
│   ├── architecture/         # Implemented chat/work memory design
│   ├── team/                 # Owners, current status and future plan
│   └── design/               # Broader design references
└── tests/                    # Desktop, storage, protocol and interaction smoke projects
```

Gateway unit/integration tests are in `apps/model-gateway/tests/`. Future `apps/host/`, `crates/` and other design directories are not part of the current repository.

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

The planned trusted authority layer will be responsible for:

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

In the future design, local models may use a governed Web Search capability. Search and capability-level network controls are not implemented yet.

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

* [Model gateway and local startup](apps/model-gateway/README.md)
* [Chat/work memory and Data folders](docs/architecture/chat-work-memory.md)
* [UI components](apps/desktop/UI-COMPONENTS.md)
* [Team plan and next steps](docs/team/README.md)
* [Captain overview](docs/team/队长总览.md)
* [R3 design reference](docs/design/r3-refined/README.md)

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

Repository coding work uses the [development Skill](.agents/skills/kynxa-development/SKILL.md) and [code standards Skill](.agents/skills/kynxa-code-standards/SKILL.md), automatically referenced by `AGENTS.md`. The latter maintains semantic naming, English/Chinese explanatory comments, language-specific formatting and compatibility exceptions; protocol keys and third-party originals remain unchanged. These are repository development instructions, separate from the application's model skills.

The [five-person development guide](docs/team/README.md) and [team lead overview](docs/team/队长总览.md) (Chinese) define the implemented module ownership, interfaces, and acceptance criteria. The lead is one of the five people. [GitHub collaboration](docs/team/github-collaboration.md) provides collaborator invitations, CODEOWNERS generation and main-branch protection setup; directory ownership is a review responsibility, not a folder-scoped Git permission.

Bug reports, architecture discussions, implementation proposals, tests, documentation improvements, and security reviews will be welcome.

## License

License information will be finalized before the public release.

## Disclaimer

KYNXA is experimental software.

Agent systems may interact with files, processes, browsers, external services, and other sensitive resources. Production releases must enforce explicit capability boundaries and should never treat model output as trusted authority.
