# Transcript UI smoke

Run the real WinUI/WebView2 fixture from the repository root on Windows:

```powershell
dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false
```

The default entry in `App.xaml.cs` runs the complete DOM suite, including cold/warm formula rendering, native HTML tables, exact code and TeX copying, cross-message selection, live language changes, conversation switching, tool presentation, completed/streaming states, scroll anchors, reply actions, per-message time metadata and copy feedback, multiline LF/CRLF replies, 480/980/1600 width checks, long inline math, monotonic live reply timing and retrieval tool presentation. It links the production transcript control, Markdown rendering, view models, JavaScript, CSS and KaTeX resources.

To run only the long inline formula checks:

```powershell
dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false -- --inline-math-only
```

`App.InlineMathChecks.cs` uses actual rendered KaTeX DOM at 480 and 980 DIP. It verifies that a short formula stays inline with surrounding text, an oversized formula scrolls within the reply without widening the page, its final term can be reached, and selecting the complete formula returns the exact original TeX with delimiters. It does not substitute a screenshot or a mock HTML renderer for those checks.

`App.MessageMetadataChecks.cs` checks the production message `title` and accessible description with synthetic user, streaming, completed, cancelled, failed and historical message states. Sending/creation dates use the local time zone in `yyyy/MM/dd HH:mm:ss` form; ending dates are explicitly marked as local terminal observations. Historical messages without such an observation display “not recorded”. The fixture deliberately supplies an observed end different from creation plus duration, verifies first-observation stability and same-ID/new-object isolation, changes only a creation date to check metadata refresh without body replacement, and checks localization of active and detached cached messages without changing their source or selection. These state transitions use the presentation helper; they do not claim to exercise a real HTTP or model stream.

The same check invokes an actual message copy through the native host, compares clipboard Markdown/Unicode/TeX/indentation exactly, and observes the real acknowledgement SVG. The feedback stays visible after the owned WebView pointer moves away and clears after approximately 2200ms. Time metadata must never enter selected or copied message text. `kynxa-transcript-message-copy-feedback.png` in `%TEMP%` captures the acknowledgement state.

To run only the multiline reply checks:

```powershell
dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false -- --multiline-only
```

`App.MultilineChecks.cs` verifies actual glyph positions for 20 separate numbered lines at 480/980/1600 widths, including LF and CRLF sources. It compares selected-text copying exactly with LF lines and native whole-message copying with the original LF/CRLF source. Streaming chunks include a CRLF split across chunks and finalization; a single-line `001002` control must remain on one baseline. These are synthetic presentation updates, not a real model or HTTP stream. The preview is `%TEMP%/kynxa-transcript-multiline.png`.

The fixture creates synthetic messages and sets `KYNXA_DATA_HOME` to a unique temporary directory before creating the production control. It does not read user chats, connect a model, submit approvals, or execute tools. The production control's WebView2 cache is used unless the launching environment explicitly overrides its user-data folder.

Results are written to `%TEMP%/kynxa-transcript-smoke.txt`. The complete suite also writes `%TEMP%/kynxa-transcript-smoke.json` and preview PNGs; the focused inline-math run writes `kynxa-transcript-inline-math-480.png` and `kynxa-transcript-inline-math-980.png` in `%TEMP%`. Read the current result file after each run; a screenshot alone does not indicate a passing suite.

Pass `--keep-open` after `--` to retain the fixture window for inspection. The optional `--pointer` flag adds the WebView2 CDP drag/selection diagnostic to the complete suite. Run window checks separately from other native UI fixtures.
