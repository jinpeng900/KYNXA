# Agent native UI smoke

Run from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tests/agent-ui-smoke/run.ps1
```

The fixture links the production tool management window, approval dialog, localization and message presentation code. It uses a fake Agent API and a unique temporary data directory, captures its own window, and closes only that window. It does not connect to configured MCP processes or modify the running production application. Run native UI fixtures sequentially.

Checks cover configuration conflicts, explicit saves, deletion confirmation, skill previews and late results, language changes, MCP partial failures, approval decisions, cancellation and retry visibility. Results and screenshots remain in the temporary directory reported by the runner. The full ShellPage settings entry is checked through its production event wiring; this fixture hosts the management window directly.
