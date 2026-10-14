# Transcript UI smoke

## Current conversation body search

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario body-search
```

`App.BodySearchChecks.cs` exercises the production `OpenFindText` / `CloseFindText` commands in real WinUI/WebView2 with synthetic messages. It checks Chinese/English literal matching across styled spans, code/table/quote/Mermaid source, complete counts, explicit navigation, folded content reveal, non-mutating CSS Highlight ranges, native clipboard and consecutive blank lines, selection protection, cached A/B/A reading positions, streaming and rapid edits, Unicode offsets, localization, themes and native viewport sizes. It is also included in `all`; operating-system Ctrl+F / Ctrl+Shift+K and modal focus are covered by the Shell fixture. Actual results come from each run's result files, not this description.

`App.NativeMergeChecks.cs` is included in `body-search` and `all`. It uses two synthetic 64-message conversations to check complete search counts across the currently loaded virtualized page, restored DOM recycling after closing search, wrap/fold preferences across actual unmount/remount, A/B/A visible reading anchors and the native latest/older history events. These checks make no model calls and do not claim to search unloaded durable history.

`App.NativeMergeChecks.cs` 归属 `body-search` 和 `all`：用两个各 64 条消息的隔离会话，检查当前已加载页的完整搜索、关闭搜索后恢复 DOM 回收、真实卸载重挂后的换行/折叠偏好、A/B/A 可见阅读锚点，以及原生最新页/更早历史事件。不调用模型，也不将当前页搜索描述为未加载历史的全文搜索。

Additional boundary checks span actual inline KaTeX's visual text while excluding its hidden MathML, and verify short-document matches are not obscured by the floating bar after explicit navigation. A controlled scheduling stress case uses 8000 synthetic DOM paragraphs in the real WebView, four-times CPU throttling and 80 updates at a requested 16 ms interval; it requires useful results before updates finish and an exact final count of 8080. Captured bodies are removed or hidden during that scan to exercise stale-node guards. This isolates search scheduling from Markdown parsing and does not claim an equally large generated Markdown conversation or a real provider throughput benchmark.

## Native render performance

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario render-performance
```

This selects the existing `--render-performance-only` entry in `App.xaml.cs`. It checks bounded message-body DOM, changed-row transport, selected patch convergence, streaming stable-prefix reuse and user-driven native history admission using synthetic conversations. The focused metrics are written to `kynxa-transcript-performance.json` inside the run's isolated `temp` directory; the normal fresh `kynxa-transcript-smoke.txt` remains the pass/fail result. It is a presentation regression check, not a real model or gateway throughput benchmark.

该选项接入现有 `--render-performance-only` 分支，用隔离消息验证有界正文 DOM、单行增量传输、选区期间更新收束、流式稳定前缀复用及用户触发的原生历史加载。指标保存在本轮独立 `temp/kynxa-transcript-performance.json`，通过/失败仍以本轮 `kynxa-transcript-smoke.txt` 为准，不代表真实模型或网关吞吐量测试。

Run the real WinUI/WebView2 fixture from the repository root on Windows with PowerShell 7 and the Visual Studio build tools for WinUI development installed:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario all
```

On the current development machine, PowerShell 7 is supplied by Codex's bundled runtime. If a normal Windows terminal cannot find `pwsh`, run the same entry from the repository root with its verified local path:

```powershell
& "$env:USERPROFILE\.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe" -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario format
```

Other machines can use their installed PowerShell 7. These commands do not change Windows execution policy.

The default entry in `App.xaml.cs` runs the complete DOM suite, including cold/warm formula rendering, native HTML tables, exact code and TeX copying, cross-message selection, live language changes, conversation switching, tool presentation, completed/streaming states, scroll anchors, reply actions, per-message time metadata and copy feedback, local end-time cache restart recovery, multiline LF/CRLF replies, 480/980/1600 width checks, long inline math, monotonic live reply timing and retrieval tool presentation. It links the production transcript control, Markdown rendering, view models, JavaScript, CSS and KaTeX resources.

`run-checks.ps1` selects Visual Studio's amd64 MSBuild **and its .NET Framework C# compiler**, fixes Debug/x64/win-x64 and disables reusable/concurrent build nodes. Full MSBuild alone still selected the SDK's .NET 10 C# compiler on this machine, which suffered a separate CLR internal failure during source generation. The runner explicitly supplies and verifies `CscToolPath`/`CscToolExe` for the installed Visual Studio compiler; the project's .NET 10 target and application runtime remain unchanged. Its intermediate files and binaries are isolated under the ignored `artifacts/transcript-ui-smoke/vs-framework-csc-x64/` directory. The project also excludes historical `obj/` and `bin/` files from source discovery when using that external intermediate path. The runner builds first, resolves the evaluated executable path and starts that executable directly. It never accepts a pre-existing executable or an old result as proof that a new build passed. Each run has its own temporary directory, WebView2 profile, result and log; environment overrides are scoped to the runner and its child processes.

The examples include the optional `-SingleProcessor` compatibility setting tested on the development machine. It temporarily restricts the runner and its build children to the first permitted logical processor; Windows child processes inherit the parent's affinity. The original affinity is restored before starting the native fixture and in failure cleanup. It changes no machine-wide CPU setting or BIOS configuration, and can be omitted on machines with a stable build environment. Four consecutive full rebuilds passed with this setting after unrestricted builds had intermittent native CLR failures; the fourth also passed the 88 native format checks, with processor affinity and all five process environment values restored exactly afterwards. That is evidence for a local workaround, not a diagnosis of faulty hardware or a permanent CLR repair.

Use `-BuildOnly` to compile without opening the fixture, and `-Rebuild` to rebuild this runner's isolated artifacts. For normal validation the runner checks both the process and a fresh `PASS:` result; an internal fixture exception written as `FAIL:` is a failure even if the WinUI process exits with code zero. A compiler error stops the run before launching the fixture, with no automatic retry. Keep other native clipboard/window fixtures separate. This entry affects this test project only; it does not change the desktop application's build settings, gateway, SDK versions, Windows policy or NuGet audit settings.

To run only the long inline formula checks:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario inline-math
```

`App.InlineMathChecks.cs` uses actual rendered KaTeX DOM at 480 and 980 DIP. It verifies that a short formula stays inline with surrounding text, an oversized formula scrolls within the reply without widening the page, its final term can be reached, and selecting the complete formula returns the exact original TeX with delimiters. It does not substitute a screenshot or a mock HTML renderer for those checks.

To run only the persistent message time and copy feedback checks:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario metadata
```

`App.MessageMetadataChecks.cs` checks the production `time.message-time` beside the copy icon with synthetic user, streaming, completed, cancelled, failed and historical message states. The footer stays visible without hover or focus, and the message article has neither a `title` nor an `aria-description` containing the removed hover metadata. The fixture checks actual copy/time geometry at native widths 480 and 980, their DOM order, no clipping or page widening, and omission from selection/copy. Chinese text distinguishes `发送时间`, `回复完成时间` and `回复结束时间`; ongoing replies show `正在生成`. Known times use local `yyyy/MM/dd HH:mm:ss` text and an ISO `datetime` attribute. Unrecorded historical ends display `未记录` without a fabricated `datetime`, and cancellation/failure never uses the successful completion label.

The fixture deliberately supplies an observed end different from creation plus duration, verifies first-observation stability and same-ID/new-object isolation, and changes only a user's `CreatedAt` to check footer refresh without body replacement. It checks localization of active and detached cached message times while preserving exact selection, original body DOM and formal message serialization. These state transitions run with the local disk cache disabled and verify the transient helper's object isolation; they do not claim to exercise a real HTTP or model stream or add persistent ending dates to historical records. The separate cache fixture below checks restored observations on fresh deserialized message objects.

The same check invokes an actual message copy through the native host, compares clipboard Markdown/Unicode/TeX/indentation exactly, and observes the real acknowledgement SVG. The checkmark clears after approximately 2200ms; the normal copy icon and time remain visible afterwards. Time text, ISO date and both positions remain unchanged through the feedback. Previews in `%TEMP%` are `kynxa-transcript-message-time-980.png`, `kynxa-transcript-message-time-480.png` and `kynxa-transcript-message-time-en.png` for a short completed two-message conversation, `kynxa-transcript-message-time-states-980.png` / `-480.png` for all terminal states, and `kynxa-transcript-message-copy-feedback.png` for the acknowledgement. The focused run writes the current geometry and time evidence to `kynxa-transcript-smoke.json`.

## Independent content-block copy

Run the focused native WebView2 checks:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario block-copy
```

`App.BlockCopyChecks.cs` covers code, JSON, unlabelled/plain/unknown text, empty fences and indented code; each block has one persistent copy button above its source. It compares the actual native clipboard with the visible `code.textContent`, including Unicode, tabs, indentation, blank lines and literal markup. Native acknowledgement controls the checkmark, which resets after approximately 2200ms. The failure path deliberately intercepts a request and supplies a failed host receipt; this simulates feedback and retry, without claiming an operating-system clipboard failure.

Checks include cross-block selection without toolbars, unchanged whole-message Markdown copy, no duplicate Mermaid copy button, old-conversation receipts, cached DOM restoration and subsequent native copy, trusted WebView-local Tab/Enter/Space, all palettes and English labels without replacing selected prose, and actual 480/980 window geometry. Synthetic streaming updates verify copying the visible partial source while a selection holds a newer snapshot pending, then normal copying after fence closure; these do not call a real model. A trusted WebView-local CDP pointer press/release verifies clicking copy preserves that selected reading range and copies the visible partial source; it does not claim an operating-system mouse drag. Formal message JSON must remain unchanged. Previews are `%TEMP%/kynxa-transcript-block-copy-480.png` and `-980.png`; the focused run writes the standard current result and metrics files. The complete suite also includes this check.

The block-copy fixture also verifies visible local failure messages for ordinary blocks, whole-message copy and Mermaid, with no success/failure `title` tooltip. Synthetic failure receipts do not force an OS clipboard error. The isolated page uses [CDP focus emulation](https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setFocusEmulationEnabled) during its keyboard/pointer/selection checks and disables it afterwards; this keeps the page active without taking the user's OS window focus. Production copy no longer emits a page-wide feedback event: tests await actual `copyResult` or the acknowledgement-driven local state and compare the native clipboard.

## Table copying and long-content reading

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario content-reading
```

`App.ContentReadingChecks.cs` uses the production transcript in a real WinUI/WebView2 window. It checks independent table copying as Excel-friendly TSV and Markdown, native Windows clipboard contents, acknowledgement-driven local success/failure, retry, late previous-conversation receipts, header-only/empty tables, Chinese/English cells, escaped pipes and original TeX. Safe DOM construction additionally checks a semantic cell line break and quotes; this is a serializer boundary fixture, not acceptance of model-supplied HTML. The Markdown renderer's HTML restriction remains in force.

The same focused entry checks explicit source/wrap switching, source-line counts, manual long-block folding, full-source copy from a folded preview, selected-text protection with a local explanation, trusted keyboard Space/Enter, live language changes, cached reading preferences, and synthetic streamed fence completion. It checks native 480/980 geometry, menu keyboard focus and Escape, and scrolling to the final column of a wide table. These synthetic messages use isolated data and do not call a real model or backend. Failed clipboard receipts are simulated; real operating-system clipboard failure is not forced.

The reading-position regression navigates through the public C# host API with its default `openAtBottom=true`: wrap/fold in A, visit a short B, and return to the same reading region in A. It compares both scroll position and the original block's viewport anchor, then checks a late host refresh, layout notifications, native window resizing, rapid B/C/A switches, explicit latest navigation and continued bottom following. The same fixture supplies distinct LF 10-line, LF 9-line and CRLF 10-line sources and splits a synthetic stream inside CRLF pairs; rendered text, displayed line counts, selected source, actual block clipboard and whole-message clipboard must agree with their respective sources. It does not insert blank lines that were absent from the model's source.

Wide-table checks compare real cell and glyph geometry: 16 columns keep a 10rem minimum, English tokens and final-column markers stay intact, horizontal scrolling reaches the final cell, and the toolbar remains fixed. Two/three-column controls keep their responsive layout, including short math, long prose and unbroken words. Requested native widths are 480/980/1600/2400; the metrics record actual WebView widths, which can be clamped by the display's available area. A requested 2400 width alone does not establish a 2400-pixel viewport.

The runner saves fresh result/metrics files and table/long-content preview PNGs under its current isolated evidence directory. The default `all` suite includes this group. A build or screenshot alone does not establish a passing run.

Validation (2026-10-09): the final resources passed 137 native `content-reading` checks, 155 `block-copy` regression checks and 88 `format` checks, with zero build warnings/errors in each run. Current evidence is under `artifacts/transcript-ui-smoke/vs-framework-csc-x64/runs/20261009-221400-content-reading-59a2edcf`, `20261009-221425-block-copy-f50f5ba4` and `20261009-221514-format-307f7c01`. The folded preview additionally checks actual source glyphs against `clientHeight`, excluding the horizontal scrollbar: line 12 stays completely visible and line 13 does not enter the source viewport. The 480-DIP source/wrapped screenshots were inspected.

One earlier block-copy run (`20261009-220646-block-copy-0e3e12e3`) received a real failed native clipboard receipt on its final streamed-block copy, after preceding copies succeeded; its local error feedback was correct. Two later focused block-copy runs passed. The fixture now records only caught clipboard-related exception type/HResult/message through a scoped first-chance observer, removed in `finally`; the later runs recorded none. The original failure's specific exception was not recorded, so these results neither identify a unique operating-system cause nor establish a permanent clipboard repair. The failed run remains preserved and is not counted as passing.

Validation follow-up (2026-10-10): the final resources passed 252 `content-reading`, 88 `format` and 94 `diagrams` native checks, with zero build warnings/errors for each fixture. The runs are `20261010-095608-content-reading-8a7af885`, `20261010-095708-format-842a3f06` and `20261010-095748-diagrams-1e37cd82` under the same evidence root. Actual wide-table viewports were 464/964/1584/1711 CSS pixels; the 2400 requested window was constrained by this display. A controlled old-script run reproduced the A/B/A bottom jump; the fixed run preserved scrollY=709.3333 and blockTop=90.1667 exactly with wrap/fold enabled and following disabled. An initial all-table minimum-width version failed the existing narrow three-column format assertion (`20261010-095032-format-81251033`); restricting the minimum to four or more columns preserved that assertion in the final rerun. Both failure records remain available and are excluded from the 434 passing checks. This follow-up does not claim to rerun the default `all` suite or call a real model.

## Content-block formatting and horizontal scrolling

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario format
```

The focused entry checks ordinary code surfaces at 480/980/1600 native window widths: original source lines remain intact, long lines scroll inside their own frame, and prose/table wrapping does not widen the page. It also checks the requested `shell`, empty `txt`, single long `txt` line, ordinary Chinese/English prose, multiline blockquote, literal special symbols and consecutive blank-line cases. Original code text and native copied text must agree; ordinary paragraphs and quotes must keep their semantic form. The fixture uses synthetic messages in its isolated data root and makes no model requests. This group is also included in the default suite.

Validation update (2026-10-09): the complete C# fixture now builds with Visual Studio amd64 MSBuild, and the new focused entry has actually passed 88 checks, including native window resizing at 480/980/1600 and the real Windows clipboard. A fresh isolated copy also compiled with zero errors; two NU1900 warnings reported a TLS/network failure reaching NuGet's vulnerability feed. Those warnings are recorded separately, without disabling the audit. The previous 49 CDP-only checks remain historical supplementary evidence, not a substitute for this native run or a claim that every focused/default suite was rerun.

The earlier failure occurred inside the WinUI XAML compiler while reading assembly metadata (`WMC9999`, `Module(0x0)`); one compiler process exited with `0xC0000005`. The later missing `App.xbf` was a consequence of that failed compilation. The saved assemblies and all 206 references could subsequently be read by the same-version metadata reader, and the unchanged source rebuilt successfully. A later repeated build also exposed a separate `0x80131506` failure in the SDK .NET 10 C# compiler's incremental source-generator execution, even with isolated files and Full MSBuild. The Visual Studio C# compiler bypassed that distinct SDK compiler path, but XAML failure recurred on an unrestricted rebuild; diagnostic JIT/NGEN overrides also failed to establish stability and are not retained. Windows events confirm native access violations across the relevant compiler processes. The documented single-processor entry is the currently tested local compatibility option; it verifies fresh results without retrying or suppressing errors. These observations do not prove a unique cache corruption cause or a permanent system-wide CLR repair. See the UI validation record for the evidence paths.

## Real API final-answer rendering

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -Scenario api-final -ApiReportPath '<absolute acceptance report path>' -TimeoutSeconds 240
```

This entry reads a previously saved real API report; it does not call a model or modify that report. It requires an actually received `completed` event with a completed `final_answer`, then opens its original segments in the production native WebView. Checks compare the visible Markdown, marked answer continuations, native clipboard, production export, stopped timer and reopened conversation. Screenshots and DOM evidence are saved in isolated test directories. A replayed partial outcome checks that the same answer remains visible, without changing the API task's result. An interrupted response cannot pass this entry simply because it contains progress text.

## Local end-time cache

Run the focused production cache and native WebView2 restoration checks:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario message-time-cache
```

`App.MessageTimeCacheChecks.cs` records and saves a synthetic terminal observation, flushes pending writes, discards all process observations, and deserializes a new message from the original formal JSON. Opening that message through the production `ConversationTranscript.ShowConversation` must restore the exact ISO date in the real footer without changing formal message serialization or restoring a hover popup. The fixture uses unique temporary desktop directories, does not read actual conversations or API keys, and does not make model calls. Restart recovery here is simulated by clearing process-local helper state while retaining the written files; it is not a claim that this fixture relaunches the entire desktop process.

Isolation checks cover the same message ID in different conversations and data roots, changed final content/status, active generation, user messages and legitimate client/gateway clock offsets. An explicit same-ID/same-content retry must persist a new end; deleting one conversation must leave another intact, while undo resaves the original live object's observation for later restart recovery. A file in place of the desktop directory checks write failure without losing the currently displayed time or changing model state. Rapid conversation switching checks that late cache reads cannot overwrite the current document.

This focused run does not require clipboard access, native keyboard input or foreground activation. It writes the normal result and metrics files and places `message-time-restored.png` inside its unique `%TEMP%/kynxa-message-time-cache-...` directory, recorded as `messageTimeCacheRestoredPreview` in the metrics. The complete suite includes these checks as well.

To run only the multiline reply checks:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario multiline
```

`App.MultilineChecks.cs` verifies actual glyph positions for 20 separate numbered lines at 480/980/1600 widths, including LF and CRLF sources. It compares selected-text copying exactly with LF lines and native whole-message copying with the original LF/CRLF source. Streaming chunks include a CRLF split across chunks and finalization; a single-line `001002` control must remain on one baseline. These are synthetic presentation updates, not a real model or HTTP stream. The preview is `%TEMP%/kynxa-transcript-multiline.png`.

The fixture creates synthetic messages and sets `KYNXA_DATA_HOME` to a unique temporary directory before creating the production control. It does not read user chats, connect a model, submit approvals, or execute tools. The runner explicitly supplies a dedicated WebView2 user-data folder for each run.

The fixture writes `kynxa-transcript-smoke.txt` into its process-local `%TEMP%`; the runner prints the current evidence directory under `artifacts/transcript-ui-smoke/vs-framework-csc-x64/runs/`. The complete suite also writes `kynxa-transcript-smoke.json` and preview PNGs; the focused inline-math run writes `kynxa-transcript-inline-math-480.png` and `kynxa-transcript-inline-math-980.png`. A screenshot alone does not indicate a passing suite.

For interactive inspection, use `-BuildOnly` and launch the printed executable path with `--keep-open`. The fixture's optional `--pointer` argument adds the WebView2 CDP drag/selection diagnostic to its complete suite when launched directly. The unattended runner waits for process completion and a current passing result.

## Mermaid SVG / PNG export

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario diagram-export -TimeoutSeconds 600
```

`App.DiagramExportChecks.cs` exercises the production WebView2 rendering, export preparation, native message bridge, independent SVG/PNG validation and `StorageFile` byte writer. Only the interactive save picker is replaced with a scoped delegate that writes to the run's isolated temporary directory. Success feedback must follow the actual production file write; a build, mock receipt or screenshot does not establish a successful export. This does not claim automated interaction with the Windows save dialog.

The fixture exports flowchart/graph, sequence, state, class, entity-relationship, Gantt and mind map diagrams, checking bilingual/long/multiline labels, explicit full viewBox, inert standalone SVG styles, actual PNG decoding, nonempty raster pixels and bounded image dimensions. It compares complete normalized SVG geometry before/after zoom and from fullscreen, rather than comparing only text or the visible viewport. PNG aspect checks allow only integer rounding on the short axis. Original formal message JSON must stay unchanged.

Controlled save delegates cover cancellation, explicit errors, thrown I/O errors, retry and a second graph. A delayed save checks global busy controls, native duplicate rejection, same-conversation message updates, navigation and A/B/A late-result isolation. A delayed real `canvas.toBlob` callback establishes deterministic ordering for theme changes and navigation during preparation: the saved image retains the clicked theme, and abandoned preparation cannot call the native saver. This uses the real image/canvas encoder with a held callback; it does not claim a physical timing race.

Safety probes first submit a valid payload through the same bridge schema, then reject scripts, DTD/entities, event handlers, arbitrary HTML, external images/URLs, invalid Base64, unsupported formats and malformed PNG. A synthetic large viewBox verifies that SVG keeps its full extent while PNG is bounded to 16 million pixels before image decode. It does not simulate a 100000-node renderer stress test. Additional checks reject excessive SVG bytes/depth, PNG CRC/truncation errors and extension mismatches. Incomplete and failed diagrams retain source and cannot export.

Selected prose and the rendered SVG node survive export. Exact light/dark background pixels, standalone SVG background, English names and the 480-DIP toolbar layout are checked. Export feedback stays within its diagram. Fresh files named `diagram-export-*.svg`, `diagram-export-*.png`, a 480-width preview and the last isolated synthetic bridge payload are retained under the current run's evidence directory. Font names are included in SVG; font files are not embedded, so other computers use their installed fallback fonts. These tests do not read real conversations, call a model, access remote image resources or write formal chat data. The complete `all` suite includes this group.

Validation (2026-10-11): the final focused run passed 224 native export checks, followed by 94 existing Mermaid checks. Evidence is under `artifacts/transcript-ui-smoke/vs-framework-csc-x64/runs/20261011-060618-diagram-export-64d8e137` and `20261011-060635-diagrams-15021c8e`. The final export group additionally checks the actual circular mind map label center and precise English preparation/saving/success/cancel/error/reduction text. Earlier failures exposed CSSOM marker shorthand serialization and duplicate mind map IDs; visual review found an existing circular label alignment gap and missing export localization keys. Those were fixed without relaxing native validation. The earlier 217-check result did not cover precise English feedback and is not the final result. Formal desktop Debug/x64 built with zero warnings/errors; 11 packaged transcript resources matched source SHA256. Save-dialog interaction, Release and GPU behavior were not exercised.

## Mermaid diagrams

Run the focused production-renderer checks:

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario diagrams
```

`App.DiagramChecks.cs` renders `flowchart`, its `graph` alias, `sequenceDiagram`, `stateDiagram-v2`, `classDiagram`, `erDiagram`, `gantt` and `mindmap` using the bundled runtime inside the actual native WebView2. It checks real SVG text and view boxes, Chinese/English and multiline labels, original source retention, closed-fence streaming, interrupted open fences, syntax errors and source-only size limits. The safety fixtures attempt configuration/frontmatter overrides, executable/external links, arbitrary HTML/SVG and external images; rejected diagrams retain readable source instead of creating active content.

Interaction checks use the native clipboard acknowledgement before asserting the copy checkmark, compare source exactly, resize the native window to 480/980, exercise fit/zoom and trusted WebView-local pointer panning, and close the modal viewer with an actual Escape key event. Palette and live language changes preserve message nodes, source, selection and scroll; cached and rapid conversation changes isolate asynchronous results. These are synthetic presentation inputs, not real model or HTTP streaming, and the tests do not execute diagram scripts or tools.

Cross-feature review checks cover prose-to-partial-SVG selection and the real copy event without leaking SVG styles, cached diagrams restored after changing language in another chat, mathematical comparison labels, ordinary `click`/`links` mindmap text and flowchart node IDs, and palette changes while fullscreen SVG text is selected. The last check requires the original SVG and range to remain intact until the selection is cleared, then waits for the actual new node fill in the still-open viewer.

Fullscreen copying also covers ranges whose common ancestor is the SVG itself. Both complete SVG children and a range from the SVG's first child to the middle of a label must omit style/defs even when `cloneContents()` drops the outer SVG element. These viewer selections copy visible labels; the inline whole-diagram source projection and explicit source-copy button retain their separate checks.

The focused run writes the current result to `%TEMP%/kynxa-transcript-smoke.txt` and measurements to `kynxa-transcript-smoke.json`. Its preview PNGs are `kynxa-transcript-mermaid-types.png`, `kynxa-transcript-mermaid-980.png`, `kynxa-transcript-mermaid-480.png` and `kynxa-transcript-mermaid-fullscreen.png`. The default complete suite also includes these checks. Read the current result after running; the existence of a preview is not a passing result.

## Chinese/English typography

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario typography
```

`App.TypographyChecks.cs` checks computed fonts for user/assistant text, headings, tables, timestamps, buttons and Mermaid labels inside the production WebView2. DevTools `CSS.getPlatformFontsForNode` additionally inspects the installed font faces actually used for English/Chinese paragraph glyphs and monospaced code. KaTeX math retains its own face. Native window resizing checks page width and heading hierarchy, saves `kynxa-transcript-typography-980.png` and `kynxa-transcript-typography-480.png` in TEMP, and checks fullscreen diagram text. Original formal message JSON must remain identical. The normal result/metrics files and default complete entry include this group; read the current result after running.

## Theme and reading preferences

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario appearance-reading
```

`App.AppearanceReadingChecks.cs` applies the production preferences to the real WinUI/WebView2 control. It covers five palettes in light/dark modes; 13, 13.5, 15, 18 and 22 px chat sizes; semantic prose, code highlighting, formulas and table colors; exact selected text and existing DOM references; twelve complete folded preview lines; full native clipboard copying; cached A/B/A reading anchors; safe redraw of the seven Mermaid types; and the actual system-mode preference. High contrast uses a synthetic system-color payload and supported current-system property reads, without changing Windows settings.

A deterministic ordering check delivers keyboard/wheel navigation between the real host appearance message and its deferred animation frame. The new navigation must win over font anchor restoration. This exercises the production navigation handlers with synthetic events; it does not claim physical keyboard or mouse input. The source messages remain byte-for-byte equivalent as serialized formal objects.

The focused run writes current PASS/FAIL and measurements under the runner's unique evidence directory, including `kynxa-transcript-appearance-reading-480.png`, `kynxa-transcript-appearance-reading-980.png`, `kynxa-transcript-appearance-high-contrast.png` and `kynxa-transcript-appearance-dark-diagrams.png`. Read the current result; previews alone are not verification. Native settings persistence, rollback, color picking and shared icon brushes are covered by the separate Shell fixture.

## Copy lifetime and rapid language/navigation regression

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario clipboard-persistence
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario language-navigation
```

The clipboard scenario uses two sequential native processes. The writer copies synthetic Unicode, indentation and blank lines through the production content-block bridge, waits for its actual receipt and exits without reading the clipboard. Only then does a fresh reader check the advertised text format and exact source. Both process exit codes and fresh results must pass; the writer/reader evidence is retained separately. This test uses the real Windows clipboard and overwrites its current contents.

The language/navigation scenario preserves strict source HTML, every source node, selected endpoints, exact selection-copy text and scroll position while the application-owned block toolbar translates. It also exercises twelve rapid native language/navigation transitions without a resize or withheld animation frame. A mounted row must follow the new conversation identity rather than remaining an empty virtual placeholder. The body-search scenario additionally verifies that a foreground table menu consumes the first trusted Escape while the search bar stays open, with IME and focus restoration preserved.
