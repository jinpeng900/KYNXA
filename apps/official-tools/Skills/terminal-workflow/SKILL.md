---
name: terminal-workflow
description: Use the right workspace's interactive terminal or bounded sandbox/host commands, inspect local command environments, and verify actual output.
license: Apache-2.0
compatibility: Sandboxed commands need the verified Windows AppContainer; host commands need the native host terminal. No Python is required for this skill.
metadata:
  capability: terminal
  runtime: native
---

# Terminal workflow

Use this skill for local commands, environment diagnostics and tests. It provides execution guidance, not an additional terminal or a permission bypass. Discover current tools with `tool.search`, then `tool.load` for deferred exact names; older unavailable reports do not establish current capability.

## Choose the execution boundary

- `terminal.workspace.list` finds this chat's right-side terminal sessions, including terminals opened by the user. `terminal.workspace.open` reuses a session or opens an embedded terminal in the current work directory; use `newSession: true` only when another session is needed. Keep its returned `sessionId` instead of opening a separate terminal window.
- For a multi-folder project, `work.folder.list` returns authorized `mountId`/name/path values and the default root without waiting for indexing. Pass the chosen `mountId` when opening a terminal for a different root; files with the same relative path in different mounts are distinct. A terminal keeps its own working directory even when the project's default root changes; never infer access to their shared parent.
- `terminal.workspace.write` sends actual input to that same interactive shell. Append `\r` to execute a command; use `terminal.workspace.read` with the returned output cursor (`after`) and a bounded `waitMs` to observe actual output. Input accepted does not mean command succeeded. Do not type into a busy user session blindly or close a user's terminal merely because your step finished; choose an appropriate free/new session. `terminal.workspace.close` ends that session when requested.
- `terminal.run` uses a temporary work snapshot inside the verified AppContainer. It supports the current declared restricted CMD commands, without network or automatic write-back. It cannot read the user's installed Conda, ordinary home directory or other host programs. A sandbox failure is not proof that the host executable is absent.
- `terminal.host.run` runs real CMD or PowerShell with the host environment, outside that sandbox. Choose it explicitly when the user asks to use an installed command such as Conda, inspect host processes or run a host-dependent task. Give a concrete `reason` and follow Ask/Smart approval. Do not claim host execution is protected by AppContainer.
- `terminal.host.start` starts a controlled hidden background command when the task should outlive one model round. Its default deadline is 30 minutes, with a six-hour maximum. Use `terminal.host.read` to inspect paged output and the actual running/completed/error state, and `terminal.host.stop` to stop the owned process tree. Job IDs belong to this chat; another chat cannot read or stop them. Starting and stopping host effects follow the current approval policy.
- A conversation without a linked work folder has its own app-managed persistent work directory. Relative file tools use that directory; terminal execution uses the corresponding selected boundary. Do not require the user to mount a project merely to diagnose a host command.
- Default to captured background execution. Use `visible: true` only when the user asks for a separate visible terminal. It opens a real console with a bounded screen preview, not a right-sidebar terminal or a persistent session. Keep its display hold within the command timeout. Never substitute `computer.launch`, shell `start` or an untracked detached process.

## Run a scoped command

1. Select `shell: "cmd"` or `shell: "powershell"` for the host tool, and an existing absolute `cwd` when the task requires one. Use the current work directory by default; an external directory needs its own task reason and the application's approval decision.
2. For a command that may be absent from PATH, inspect it with a scoped host command, such as CMD `where conda` or PowerShell `Get-Command conda -ErrorAction SilentlyContinue`. Use a location actually returned by discovery or supplied by the user. Do not scan every drive or guess a private installation path.
3. For Conda package inspection, use the requested environment explicitly, for example `conda list --name base --json`. Interpret the actual output and exit code. If the user's PATH has no Conda entry, report that fact and use a verified installation path when available; do not invent an environment list from cached knowledge.
4. `terminal.host.run` and sandbox calls create a new bounded shell; a prior `conda activate`, changed directory or variable does not carry across those calls. A right-workspace terminal persists shell state within its same `sessionId`; read its state/output before relying on earlier initialization. Do not assume state transfers to a different session.
5. Keep ordinary `terminal.host.run` and sandbox calls within their supported timeout, at most 120 seconds. For a longer explicit host task, use `terminal.host.start` with an appropriate bounded deadline. Background jobs are noninteractive: put initialization and the entire command in the submitted script, without relying on later stdin or a durable interactive shell. Node scripts are not sandbox commands in the native package; do not add Node back through another execution path. Sandboxed CMD uses `command: "cmd"` and `args: ["/d", "/c", "command text"]`; use filesystem tools for directory enumeration when sandbox CMD access is insufficient. PowerShell and Python are not sandbox commands.

## Preserve and verify the result

- Read the execution receipt: completion, exit code, output, timeout/cancellation and the execution boundary. A nonzero exit is a completed failed command, not a hanging task. A successful shell exit does not by itself prove the requested application or test succeeded.
- A visible console's screen preview is distinct from captured stdout/stderr and can be bounded. If a saved tool result is truncated, use its returned opaque reference with `tool.result.read` for more pages. Do not reconstruct or claim to have read omitted output.
- After a timeout, cancellation or `unknown` outcome, inspect the affected files or processes before another effect. Commands may already have changed the host. Do not rerun a write, installation or launch blindly.
- Use conflict-aware file tools for ordinary edits instead of hiding a write in a terminal command. A sandbox modification remains in its temporary snapshot; report that it was not applied to the user's files.
- A successful `terminal.host.start` receipt establishes startup, not task completion. Read the job's current state and output before reporting success. Use the returned `nextOffset` for output paging and respect `earliestOffset`/`truncated` when older bounded output is unavailable. Avoid repeatedly polling without a task need.
- A returned `starting` job is still accepted and may be waiting for resources. Keep its `jobId` and inspect that job when needed; do not start a duplicate command because the initial observation window ended. Resource waiting does not prove the command has run.
- Sandboxed runs and managed background jobs can truncate retained output while the command continues. Check the real exit code, completion and requested files; truncated or discarded bytes are not recoverable merely by paging a saved receipt. CPU and memory controls are program-enforced budgets, not permission to change the execution boundary.
- Background jobs survive completion of the model request but end at their deadline, explicit stop, or gateway shutdown. Gateway shutdown cleans their process trees; restarting the gateway does not replay commands or restore an interactive session. Do not promise durable unattended monitoring or detach work to evade these lifecycle limits.

Report the command's actual result, what it established and any remaining limitation. Preserve user changes and never expose credentials in diagnostic output or summaries.
