---
name: terminal-workflow
description: Choose sandbox or host terminal execution correctly, inspect local command environments, run bounded diagnostics or tests, and verify command outcomes.
license: Apache-2.0
compatibility: Sandboxed commands need the verified Windows AppContainer; host commands need the native host terminal. No Python is required for this skill.
metadata:
  capability: terminal
  runtime: native
---

# Terminal workflow

Use this skill for local commands, environment diagnostics and tests. It provides execution guidance, not an additional terminal or a permission bypass. Discover current tools with `tool.search`, then `tool.load` for deferred exact names; older unavailable reports do not establish current capability.

## Choose the execution boundary

- `terminal.run` uses a temporary work snapshot inside the verified AppContainer. It supports the current declared Node.js and restricted CMD commands, without network or automatic write-back. It cannot read the user's installed Conda, ordinary home directory or other host programs. A sandbox failure is not proof that the host executable is absent.
- `terminal.host.run` runs real CMD or PowerShell with the host environment, outside that sandbox. Choose it explicitly when the user asks to use an installed command such as Conda, inspect host processes or run a host-dependent task. Give a concrete `reason` and follow Ask/Smart approval. Do not claim host execution is protected by AppContainer.
- A conversation without a linked work folder has its own app-managed persistent work directory. Relative file tools use that directory; terminal execution uses the corresponding selected boundary. Do not require the user to mount a project merely to diagnose a host command.
- Default to captured background execution. Use `visible: true` only when the user asks for a separate visible terminal. It opens a real console with a bounded screen preview, not a right-sidebar terminal or a persistent session. Keep its display hold within the command timeout. Never substitute `computer.launch`, shell `start` or an untracked detached process.

## Run a scoped command

1. Select `shell: "cmd"` or `shell: "powershell"` for the host tool, and an existing absolute `cwd` when the task requires one. Use the current work directory by default; an external directory needs its own task reason and the application's approval decision.
2. For a command that may be absent from PATH, inspect it with a scoped host command, such as CMD `where conda` or PowerShell `Get-Command conda -ErrorAction SilentlyContinue`. Use a location actually returned by discovery or supplied by the user. Do not scan every drive or guess a private installation path.
3. For Conda package inspection, use the requested environment explicitly, for example `conda list --name base --json`. Interpret the actual output and exit code. If the user's PATH has no Conda entry, report that fact and use a verified installation path when available; do not invent an environment list from cached knowledge.
4. Each call creates a new bounded shell. A prior `conda activate`, changed directory or shell variable is not guaranteed to survive into the next call. Put dependent initialization and execution in the same approved script, or use the program's explicit environment argument.
5. Keep the command within the supported timeout, at most 120 seconds per call. For sandbox Node tests include `--test-isolation=none`. Sandboxed CMD uses `command: "cmd"` and `args: ["/d", "/c", "command text"]`; use filesystem tools for directory enumeration when sandbox CMD access is insufficient. PowerShell and Python are not sandbox commands.

## Preserve and verify the result

- Read the execution receipt: completion, exit code, output, timeout/cancellation and the execution boundary. A nonzero exit is a completed failed command, not a hanging task. A successful shell exit does not by itself prove the requested application or test succeeded.
- A visible console's screen preview is distinct from captured stdout/stderr and can be bounded. If a saved tool result is truncated, use its returned opaque reference with `tool.result.read` for more pages. Do not reconstruct or claim to have read omitted output.
- After a timeout, cancellation or `unknown` outcome, inspect the affected files or processes before another effect. Commands may already have changed the host. Do not rerun a write, installation or launch blindly.
- Use conflict-aware file tools for ordinary edits instead of hiding a write in a terminal command. A sandbox modification remains in its temporary snapshot; report that it was not applied to the user's files.
- These commands are bounded operations, not a durable background monitor or persistent interactive shell. Do not promise monitoring after the tool returns, or detach work to evade timeout and process-tree cleanup.

Report the command's actual result, what it established and any remaining limitation. Preserve user changes and never expose credentials in diagnostic output or summaries.
