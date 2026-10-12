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

Updated 2026-10-12. The native backend migration is in the workspace; its build and verification records are separate from earlier releases. The larger runtime design remains a plan.

* Windows desktop: C# / WinUI 3 / XAML, with WebView2 for selectable Markdown, code highlighting and KaTeX math.
* A native Rust gateway connects cloud and local HTTP model services using Chat Completions, Responses, Claude Messages and Ollama. The core has no Node dependency or JS gateway; a separate pinned private Node runtime and installed MCP packages are bundled for external tools. WebView rendering scripts remain frontend assets.
* Rust owns HTTP/SSE, model and MCP I/O, task/recovery state, context compaction, tools, retrieval, original JSONL and SQLite transactions. The independent Rust resource service coordinates real hardware admission. The desktop `dotnet run` command remains; `node server.mjs` is retired. See [the native gateway boundary and verification scope](docs/architecture/native-gateway.md).
* Complete conversations use stable chat IDs and append-only JSONL events. Switching providers or models retains the same history.
* The work sidebar has Recent, Projects and Tasks. Both project and task add buttons show a temporary new chat. Reselecting its project preserves input; switching projects discards an unsent draft. Sending commits the chat and updates recent activity.
* Confirmed memory has chat, project/work and user scopes. Sibling chats share confirmed work memory while their original histories remain separate. Archive, delete, undo and moving chats preserve the intended scope rules.
* A configurable Data root contains records, memory, attachments and settings. Initialization and verified migration preserve stable IDs; separate SQLite indexes cover sidebar metadata and scoped local retrieval.
* Settings include local RAG and web search globally and per work: explicit imports, automatic preparation of mounted work folders, keyword/vector retrieval and background jobs. Bundled offline multilingual embedding needs no database server. Search and original-file reads remain available before semantic indexing finishes. Greetings skip retrieval; detailed limits are in the [retrieval architecture](docs/architecture/retrieval.md).
* Context limits are resolved at runtime from verified model capabilities and actual local-service observations. Legacy context settings remain saved for compatibility; they do not grant model capacity. Output defaults to a 256K (262144) ceiling, with 4K/8K/16K/32K/64K/128K/256K and custom choices; known provider limits and available input space can reduce it. Unknown API deployments cannot have every limit predicted in advance; recognized HTTP 400 context rejections use bounded recovery within the current request.
* Chats retain all original messages; restart and retrieval are tested beyond 200 messages. Bounded requests preserve recent complete turns and relevant older constraints/code excerpts. Models can page through the current chat's original messages; tool loops compact saved-result previews without deleting records.

Settings includes memory management for chat, work and user scopes. Native regressions use isolated data and local simulated upstreams; current commands are in the [core guide](apps/agent-core/README.md). Historical checks do not certify the new gateway, every provider or a real 1M model.

Model-controlled file tools, per-request approvals, stdio / Streamable HTTP MCP, application skills and a Windows AppContainer terminal are implemented. Tools and skills settings support official service presets, environment-variable authentication references, skill import and enable switches; enabled MCP programs are trusted external processes, separate from the terminal sandbox. Full Host orchestration, crash-resumable checkpoints and automatic semantic memory remain planned. Scoped FTS/vector retrieval is now implemented. See the [agent tools guide](docs/architecture/agent-tools.md) for permissions and supported commands.

Folderless chats have persistent isolated work directories. Bundled native tools can launch GUI applications, read accessible window text, capture target windows and perform approved targeted input without requiring Python. A separate host CMD/PowerShell tool preserves actual output and execution receipts. The right sidebar uses horizontally scrollable screenshot tabs and original-pixel fullscreen viewing; new screenshots do not replace an already selected tab. Terminal output is kept in execution receipts, and an explicitly requested visible console opens separately.

Large MCP catalogs are discovered separately from the bounded model tool declarations. Individual tools can be disabled or loaded on demand. Typed and structured results are saved with chat-scoped references and paged previews; private MCP metadata stays out of model and viewer projections. Approval reasons are separate from third-party arguments, skill headers use bounded YAML 1.2 parsing, and cancellation records returned execution outcomes before stopping.

Skill resources resolve from their package root with validation and dependency diagnostics. Markdown skills, remote MCP and native executable MCP remain available. Pinned built-in Node MCP services use the private runtime and installed entry scripts without system PATH or startup npm/npx downloads; JS skills use the verified private runtime through the existing sandbox. External accounts, keys, database settings and unbundled third-party dependencies remain explicit requirements. Native CDP provides browser automation. Existing user disables remain authoritative, and configuration reads do not start services. Enabled never means ready.

Settings provide separate Data and User tools locations. Extension migration verifies copies before switching its pointer and preserves existing files. The static official package is sourced from `apps/official-tools/` and shipped under `official-tools/`; it contains no executable backend JS. Public web reading, file tools and native browser/terminal operations use the existing authorization boundaries. User extensions remain separate.

Start with the [native gateway](docs/architecture/native-gateway.md), [core guide](apps/agent-core/README.md), [chat/work memory](docs/architecture/chat-work-memory.md), [UI components](apps/desktop/UI-COMPONENTS.md) and [five-person plan](docs/team/README.md).

## Five-Person Code Boundaries

A owns the Rust gateway composition, `orchestration/` and `resources/`; C owns `models/`; D owns `tools/`, ToolHost and `apps/official-tools/`; E owns `storage/` and `retrieval/`. These native modules are under `apps/agent-core/src/`. B owns all ShellPage partials, WinUI and transcript presentation.

Desktop services use `Integration/Presentation/Models/Tools/Data`, and shared contracts use `Chat/Tools/Memory`. ToolHost uses `Desktop/Terminal/Sandbox/Native`. The production CLI is `kynxa-gateway.exe`; the desktop launch command remains unchanged. These directory boundaries do not imply a complete MVVM or assembly split.

Pure domain rules retain dependency and cycle restrictions. Explicit I/O integration modules declare their allowed domain imports in the ownership manifest; they reuse authoritative ledgers and transactions. Third-party skill text and licenses are preserved. See [boundaries](docs/architecture/team-boundaries.md) and [handoff](docs/team/README.md). Current validation is recorded separately from earlier results.

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
Rust model gateway ─────► cloud API / local HTTP model server
          ├─────────────► C# ToolHost: desktop / terminal / sandbox / native
          │
          ▼
Data: catalog + JSONL records + scoped memory
      derived context excerpts + SQLite metadata index
```

The following diagram is a historical future-runtime design, separate from the current Rust gateway and C# ToolHost. It does not establish an independent Authority in the running application.

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
* **Model gateway:** Rust HTTP/SSE, native protocol decoding, MCP/CDP, ONNX inference and parsing. Private Node is bundled only for extensions and verified sandbox scripts; the core does not depend on it. No Python runtime is shipped.
* **Local model connection:** HTTP services such as llama.cpp, Ollama and LM Studio; model inference remains a separate process.
* **Persistence:** versioned JSON metadata, JSONL conversation events and scoped memory files; SQLite metadata index.
* **Desktop communication:** local HTTP / SSE to the gateway.

The C# ToolHost provides native Windows execution. A separate Host/Authority and Named Pipes remain design references; the current implementation uses WinUI, Rust and C#.

macOS, Linux, and iOS are not part of the initial product scope.

## Repository Structure

```text
KYNXA/
├── apps/
│   ├── desktop/              # Production Windows WinUI 3 frontend
│   ├── agent-core/           # Rust HTTP/SSE, agent, models, tools, retrieval and storage
│   ├── resource-service/     # Native hardware admission and owned resource leases
│   ├── official-tools/       # Static skills, configuration presets and licenses
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

Native unit/integration tests are in `apps/agent-core/src/` and `apps/agent-core/tests/`; bundle checks are in `tests/native-runtime-smoke/`. Future `apps/host/` and `crates/` design directories are not part of the current repository.

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

* [Native gateway and local startup](docs/architecture/native-gateway.md)
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
