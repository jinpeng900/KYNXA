# Agent native UI smoke

Run from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tests/agent-ui-smoke/run.ps1
```

The fixture links the production tool management window, approval and result dialogs, localization and message presentation code. It uses a fake Agent API and a unique temporary data directory, captures its own window, and closes only that window. It does not connect to configured MCP processes or modify the running production application. Run native UI fixtures sequentially. The executable's `--official-tools-only` scenario checks enabled defaults, an explicit user disable, editing without re-enabling, restoring the enabled default, and live Chinese/English not-ready diagnostics; none of those settings operations start a connection.

Checks cover configuration conflicts, explicit saves, deletion confirmation, skill previews and late results, language changes, MCP partial failures and individual raw tool switches, gateway-provided presets and deduplication, stdio/HTTP configuration and environment references, connect/disconnect/authentication states, skill diagnostics and switches, approval wrappers and decisions, result pagination and media previews, cancellation and retry visibility. The package import button is checked here; its exact HTTP payload is covered by agent-client-smoke. The shared production data/extension storage rows are hosted with a simulated folder selection to check their identical style, cancellation, mutual button disabling, path updates and live status translation; this does not invoke the OS picker or perform a migration. Results and screenshots remain in the temporary directory reported by the runner. The full ShellPage settings entry is checked through its production event wiring; this fixture hosts the management window directly.
