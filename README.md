# KYNXA 1.8.1

<p align="center">
  English | <a href="./README.zh.md">中文</a>
</p>

**KYNXA** is a local-first personal agent application for Windows. It brings cloud and local models, durable tasks, document retrieval, tools, and a file/browser/terminal workspace into one desktop application.

**Current release: `1.8.1` · Package version: `1.8.1` · Windows x64**

Version 1.8.1 reduces unnecessary verification for ordinary deliverables, fixes closing settings during loading, and improves local-model context planning, process reuse, and tray exit. Current requirements, real results, and necessary checks guide completion. Queuing, explicit steering, pause/resume, context, successful operation receipts, and existing retrieval and cache capabilities remain in place. See the [1.8.1 release notes](docs/releases/1.8.1.md), [package release page](https://github.com/jinpeng900/KYNXA/releases/tag/v1.8.1), and [documentation index](docs/README.md). The [1.7 notes](docs/releases/1.7.md) retain the first stable release's history.

**Release/source note:** Use the matching release tag when building a published package from source; a different branch or revision may contain different implementation changes.

Validation uses focused Rust, HTTP/SSE, and Windows UI checks. These checks cover the exercised paths, not every model, website, installation scenario, or large-library retrieval workload. Earlier results remain in the [1.5.3 patch notes](docs/releases/1.5.3.md).

Standalone tests, fixtures, and evaluation scripts are kept locally and excluded from current Git revisions. A fresh clone supports production builds and the source architecture check; full test commands require the locally retained test files. See the [validation guide](.agents/skills/kynxa-development/references/validation.md).

## Capabilities in the 1.8.1 package

| Area | Current capabilities |
| --- | --- |
| Chat and models | Streaming replies, cancellation, persistent history, provider/model switching; Chat Completions, Responses, Claude Messages, and Ollama-compatible connections. |
| Long tasks | Persistent task identities, checkpoints, pause/resume, queued messages, in-task steering, subagents, artifacts, and token/time accounting. Prior output remains visible when steering starts a new reply. |
| Projects | Multiple folder mounts in one project, scoped code/file retrieval, incremental indexing, and background preparation when a project becomes active. |
| Memory | Chat, work/project, and user scopes; memory candidates, confirmation, editing, and deletion. Relevant confirmed, valid memories are selected within their authorized scope instead of injecting the whole memory store. |
| Tool experience | Persistent tool catalogs and settled success/failure/latency experience. Configuration, schema, trust, disable, and deletion changes invalidate affected entries; ordinary chat adds no tool-memory calls. |
| Documents and retrieval | Direct path reads and attachment parsing; PDF, DOC/DOCX, PPT/PPTX, spreadsheets, Markdown/text/code, images/OCR, archives, and website sources. Background indexing, E5/lexical RRF, lightweight sorting, and source references use the same scoped store. Known paths and symbols can use glob/regex search directly. |
| Document creation | Native `document.create` writes real editable DOCX, PPTX, and XLSX packages; PDF export uses the bundled Office converter. Writes return validated file sizes, hashes, and paths. XLSX formulas store supplied cached results without executing formulas. |
| Right workspace | Direct DOCX/PPTX/XLSX previews for files and attachments, cached file tabs, Markdown/math/Mermaid, code highlighting, images/SVG/isolated HTML, uncommitted Git changes, and multiple embedded terminals. |
| Websites | On-demand bundled Chromium, address or keyword navigation, back/forward/refresh, interactive pages, and a persistent application browser profile. |
| Local models | Settings-based catalog, installation, import, download pause/resume, enable/disable, deletion, and a configurable model directory. Enabled models enter the normal chat selector. |
| Skills and extensions | Relevant skill headers are selected for tool tasks and full instructions are read on demand. Native tools, stdio/Streamable HTTP MCP, and the Windows ToolHost provide desktop, terminal, sandbox, and native execution. Ordinary chat does not scan unrelated skill directories. |
| Terminal execution | `terminal.workspace.run` submits commands and scripts to the visible embedded terminal, returning a command ID, observed output, completion status, and exit code. Continued reads observe the same command instead of repeating it. |
| Resource controls | Broker-selected CPU and memory limits are applied to sandbox Windows Jobs and checked before execution. Sandbox and background host-job logs remain bounded without stopping work solely because capture fills. |

DOCX exposes its first usable page before embedded fonts and images finish; long documents scroll inside the preview and retain their reading position. PPTX supports slide navigation and zoom. XLSX supports sheet switching, typed values, common styles, merged cells, formula caches, and viewport rendering for large sheets. Switching cached files reuses their rendered DOM; background preparation can finish while another tab is active. Unsupported Office features and legacy formats retain the PDF conversion path.

Document extraction, generation, and visual rendering are separate operations. A text read does not imply full layout recovery, and spreadsheet preview does not recalculate formulas. Format-specific behavior is described in the [document guide](docs/architecture/document-ingestion.md). Website login, verification, and media availability still depend on the website and browser capabilities.

The full runtime includes the multilingual **E5 embedding model** for retrieval, separate from conversation models. E5 and lexical candidates are fused by rank using **RRF**, followed by lightweight structured sorting; **BGE is no longer bundled or required**. Existing E5 indexes remain reusable. Query, retrieval, and model-prefix caches reduce repeated work while scope and source-version checks remain effective. Large-library storage, ANN readiness, and retrieval quality have separate validation scopes; see the [retrieval architecture](docs/architecture/retrieval.md).

## Install and store your data

The [1.8.1 release page](https://github.com/jinpeng900/KYNXA/releases/tag/v1.8.1) provides these Windows x64 downloads:

| Download | Use |
| --- | --- |
| [EXE installer](https://github.com/jinpeng900/KYNXA/releases/download/v1.8.1/KYNXA-1.8.1-windows-x64-setup.exe) | Run directly to install the application and its full base runtime payload. |
| [Runtime resource pack](https://github.com/jinpeng900/KYNXA/releases/download/v1.8.1/KYNXA-1.8.1-windows-x64-resources.zip) | Supply runtime resources to a source build; this is not a standalone application or developer SDK bundle. |

The EXE installer supports application, data, and user-tool locations, Chinese/English UI, desktop shortcuts, and an optional hardware-based local chat model recommendation. The full base payload includes the native services, E5, document conversion components, private Node/MCP dependencies, and Chromium. An optional chat model is downloaded through the application's model manager after first launch.

Version 1.8.1 provides the EXE installer directly and a separate runtime resource ZIP. Online installers, checksum sidecars, and maintenance manifests are not additional public assets; maintenance tools remain available in the source.

**End users of a complete installer do not need development SDKs, Rust, Node, Python, or manual PATH configuration.** Cloud model credentials and third-party services that require accounts remain user-configured. The current installer build is not production-code-signed, and its generated EXE has not undergone an installation run; see [installer packaging](apps/desktop/Build/Installer/README.md).

New unpackaged installations default to fixed `Data/` and `Extensions/` directories beside the application, with a writable per-user installation directory by default. Installation options and settings can change these locations. Existing valid storage pointers remain in use on upgrade. App-relative locations follow a moved application folder; separately selected external locations retain their configured paths. Existing AppX/MSIX installations keep their persistent user-data locations. See [paths and migration](docs/architecture/native-gateway.md).

Model connections and local-model manifests are stored under the configured model root; local weights default to its `Local/` directory. Browser login state uses the application's persistent browser profile across chats and restarts. Closing a tab does not clear that profile; explicit site-data clearing is available. Personal browser profiles are not borrowed.

## Build from source

Source development requires **Windows x64**, a supported **.NET 10 SDK**, **Rust with the MSVC toolchain**, and **Visual Studio C++ build tools with the Windows SDK**. These requirements apply to developers, not users installing the finished application.

Run from the repository root:

```powershell
# Prepare runtime resources, build, and launch using the installed developer toolchain.
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Run

# Build without launching.
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Build
```

`-ResourcePackRoot <ZIP path>` imports an available full runtime resource pack. Offline source builds also need the appropriate SDKs and Cargo/NuGet caches; a runtime resource ZIP does not replace them. Existing prepared assets are reused. See the [build guide](apps/desktop/Build/README.md) for profiles, download sources, and offline options.

The EXE installer, installation options, and upgrade behavior are described in the [packaging guide](apps/desktop/Build/Installer/README.md).

## Architecture

```text
C# / WinUI desktop + WebView rendering
                 │ local HTTP / SSE
                 ▼
Rust gateway and Agent core
  ├─ Task orchestration, context, checkpoints, and subagents
  ├─ Cloud APIs / managed local llama.cpp / external model services
  ├─ Tools, MCP, skills, web search, and Chromium/CDP
  │    └─ C# ToolHost: Windows desktop / terminal / sandbox / native operations
  ├─ Scoped retrieval, native ONNX inference, and background indexing
  └─ Authoritative JSONL / JSON / SQLite storage
                 │
                 ▼
Independent Rust hardware resource service
```

Rust owns the production HTTP/SSE backend, model/tool I/O, task state, retrieval, and storage transactions. C# owns Windows UI and ToolHost integration. WebView JavaScript renders documents and chat; the private Node runtime serves extension compatibility. The core does not require Node or a JavaScript gateway.

Original conversation events, stable IDs, tool receipts, and source versions remain authoritative. UI caches and summaries support presentation and recovery without replacing the original records. Sensitive operations continue through the existing permission and approval boundaries. See the [native gateway](docs/architecture/native-gateway.md), [durable tasks](docs/architecture/durable-tasks.md), and [agent tools](docs/architecture/agent-tools.md).

## Repository and documentation

| Path | Responsibility |
| --- | --- |
| `apps/desktop/` | WinUI, WebView presentation, desktop clients, and build/installer scripts. |
| `apps/agent-core/` | Rust gateway; orchestration, models, tools, retrieval, storage, and resource clients. |
| `apps/resource-service/` | Native hardware observation and resource admission. |
| `apps/tool-host/` | Windows desktop, terminal, sandbox, and native execution. |
| `apps/shared/` | C# chat, memory, retrieval, and tool contracts. |
| `apps/official-tools/` | Static official skills, tool presets, and original licenses. |
| `docs/` | Maintained Markdown guides, architecture, release notes, and team instructions. |
| `tests/` | Targeted desktop, storage, protocol, packaging, and interaction checks. |

The source repository keeps maintained documentation, code, manifests, and preparation scripts. This release distributes the EXE installer and runtime resource ZIP as Release assets. Generated verification artifacts are kept outside source control; historical design archives are not required for building or installing the current application.

Useful entry points:

* [Documentation index](docs/README.md) and [1.8.1 release notes](docs/releases/1.8.1.md)
* [Core guide](apps/agent-core/README.md) and [native architecture](docs/architecture/native-gateway.md)
* [Memory and conversations](docs/architecture/chat-work-memory.md)
* [Document parsing](docs/architecture/document-ingestion.md) and [retrieval](docs/architecture/retrieval.md)
* [Desktop components](apps/desktop/UI-COMPONENTS.md)
* [Five-person ownership](docs/team/README.md), [module boundaries](docs/architecture/team-boundaries.md), and [GitHub collaboration](docs/team/github-collaboration.md)

## Contributing and validation

Use the repository's [development skill](.agents/skills/kynxa-development/SKILL.md) and [code standards](.agents/skills/kynxa-code-standards/SKILL.md), as directed by `AGENTS.md`. They cover the current languages, module responsibilities, naming, bilingual comments, compatibility, and minimum necessary validation. Application skills are managed separately from these development instructions.

Choose checks that cover the changed behavior, preserve existing local edits and formal data, and distinguish current results from historical benchmark records. Source implementation, compilation, Windows interaction, retrieval quality, and installer execution are separate forms of evidence. The [validation guide](.agents/skills/kynxa-development/references/validation.md) lists the relevant commands.

Report reproducible problems with the affected release version, action, and relevant sanitized logs. Do not include credentials, private chats, memory, or personal browser data in issues or contributions.

## License

KYNXA is licensed under [Apache License 2.0](LICENSE). Bundled third-party components and model assets retain their own licenses and notices.
