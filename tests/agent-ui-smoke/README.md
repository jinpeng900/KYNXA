# Agent native UI smoke

For the focused search, draft retention and responsive layout check, build the same project, then start `AgentUiSmoke.exe --management-layout-only`. Results use the same temporary-directory pointer. The fixture waits for the target Pivot item and a completed render frame before taking narrow-window screenshots; grid coordinates alone do not prove the selected page is visible. Native fixtures must run sequentially.

The management checks also exercise real filter-reset buttons without losing dirty drafts, a delayed configuration read with live busy status, saved-form save availability, 480 × 540 DIP short windows, scroll access to server actions, directory selection and full-path tooltips. Narrow windows move connection controls and the directory expander into their existing form scroll areas; widening restores the original owners and state. `management-short-mcp-en.png` and `management-short-skills-en.png` capture the isolated English short-window states.

`App.DialogPolishChecks.cs` checks long tool names and workspace paths in a short English approval dialog, scrollable content and no default approval. Result checks cover loading/empty copy availability, real clipboard success feedback, exact Unicode/TeX/indentation and original line endings, language changes, appended pages, bounded image decoding and narrow/wide action placement. Only native TextBox display comparisons normalize CR line endings; clipboard comparisons retain the exact loaded source. Clipboard contention and the operating-system failure branch are not simulated.

Run from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tests/agent-ui-smoke/run.ps1
```

The fixture links the production tool management window, approval and result dialogs, localization and message presentation code. It uses a fake Agent API and a unique temporary data directory, captures its own window, and closes only that window. It does not connect to configured MCP processes or modify the running production application. Run native UI fixtures sequentially. The executable's `--official-tools-only` scenario checks enabled defaults, an explicit user disable, editing without re-enabling, restoring the enabled default, and live Chinese/English not-ready diagnostics; none of those settings operations start a connection.

Checks cover configuration conflicts, explicit saves, deletion confirmation, skill previews and late results, language changes, MCP partial failures and individual raw tool switches, gateway-provided presets and deduplication, stdio/HTTP configuration and environment references, connect/disconnect/authentication states, skill diagnostics and switches, approval wrappers and decisions, result pagination and media previews, cancellation and retry visibility. The package import button is checked here; its exact HTTP payload is covered by agent-client-smoke. The shared production data/extension storage rows are hosted with a simulated folder selection to check their identical style, cancellation, mutual button disabling, path updates and live status translation; this does not invoke the OS picker or perform a migration. Results and screenshots remain in the temporary directory reported by the runner. The full ShellPage settings entry is checked through its production event wiring; this fixture hosts the management window directly.
