# Transcript UI smoke

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

## Content-block formatting and horizontal scrolling

```powershell
pwsh -NoProfile -File tests/transcript-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario format
```

The focused entry checks ordinary code surfaces at 480/980/1600 native window widths: original source lines remain intact, long lines scroll inside their own frame, and prose/table wrapping does not widen the page. It also checks the requested `shell`, empty `txt`, single long `txt` line, ordinary Chinese/English prose, multiline blockquote, literal special symbols and consecutive blank-line cases. Original code text and native copied text must agree; ordinary paragraphs and quotes must keep their semantic form. The fixture uses synthetic messages in its isolated data root and makes no model requests. This group is also included in the default suite.

Validation update (2026-10-09): the complete C# fixture now builds with Visual Studio amd64 MSBuild, and the new focused entry has actually passed 88 checks, including native window resizing at 480/980/1600 and the real Windows clipboard. A fresh isolated copy also compiled with zero errors; two NU1900 warnings reported a TLS/network failure reaching NuGet's vulnerability feed. Those warnings are recorded separately, without disabling the audit. The previous 49 CDP-only checks remain historical supplementary evidence, not a substitute for this native run or a claim that every focused/default suite was rerun.

The earlier failure occurred inside the WinUI XAML compiler while reading assembly metadata (`WMC9999`, `Module(0x0)`); one compiler process exited with `0xC0000005`. The later missing `App.xbf` was a consequence of that failed compilation. The saved assemblies and all 206 references could subsequently be read by the same-version metadata reader, and the unchanged source rebuilt successfully. A later repeated build also exposed a separate `0x80131506` failure in the SDK .NET 10 C# compiler's incremental source-generator execution, even with isolated files and Full MSBuild. The Visual Studio C# compiler bypassed that distinct SDK compiler path, but XAML failure recurred on an unrestricted rebuild; diagnostic JIT/NGEN overrides also failed to establish stability and are not retained. Windows events confirm native access violations across the relevant compiler processes. The documented single-processor entry is the currently tested local compatibility option; it verifies fresh results without retrying or suppressing errors. These observations do not prove a unique cache corruption cause or a permanent system-wide CLR repair. See the UI validation record for the evidence paths.

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
