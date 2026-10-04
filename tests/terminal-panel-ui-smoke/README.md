# Terminal sidebar verification

Run `./tests/terminal-panel-ui-smoke/run.ps1` from the repository. The fixture links the production `ConversationTerminalPanel` and `TerminalOutputState`, creates isolated temporary Data/Extensions locations, and never starts a model, gateway, or user command. It checks ownership, sequential output, bounded UTF-8 tails, console replacement, honest unknown/completion states, stop identity, closing/reopening, chat switching, selection freezing, language updates, narrow layout, canonical receipt loading, stale archive completion, and disposal.

This iteration passed 34 state/native UI checks. Screenshots and `result.txt` are written below the fixture's unique temporary folder. `%TEMP%/kynxa-terminal-panel-latest.txt` points to the latest result. These checks exercise real WinUI controls using their automation APIs; they do not bypass Windows foreground protection or inject physical input into other applications.

The optional `TerminalPanelUiSmoke.exe --live-demo --tool-host <absolute ToolHost executable>` mode streams one real, isolated PowerShell command through the native helper into the same production sidebar. Its script only prints progress and sleeps. The helper runs hidden; the fixture window remains for 30 seconds after completion, then closes. `terminal-live-demo.png` records the rendered output. Closing the fixture cleans up only its owned helper/process tree. This demo does not provide a persistent interactive shell or TTY.

Formal tool results remain gateway-owned. Display output is capped at 256 KiB per run, eight runs per chat, sixteen cached chats and 4 MiB globally. Historical receipt loading uses the existing archive API and verifies the current chat, selected run and result reference before applying a late response.
