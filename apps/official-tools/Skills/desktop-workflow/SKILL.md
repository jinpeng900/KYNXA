---
name: desktop-workflow
description: Open local applications, inspect their windows, resize and capture them, and perform authorized desktop interaction using the installed native tools.
license: Apache-2.0
compatibility: Interactive Windows desktop and available native computer tools. No Python or additional desktop MCP is required.
metadata:
  capability: desktop
  runtime: native
---

# Desktop workflow

Use this skill for local application windows and screenshots. For webpages, use the browser-workflow skill and an enabled DOM connection first. For commands, use terminal-workflow; launching a shell with a desktop application tool is not terminal execution.

Use reasonable defaults for ordinary preferences. Ask only when a necessary missing fact, material ambiguity or permission prevents the requested result. If the current request asks for a plan or explanation, provide it without launching or changing the discussed applications. Report a confirmed necessary capability limitation and reasonable alternatives rather than repeating an unavailable operation.

## Identify and observe

1. Discover current tools with `tool.search` and load deferred exact names with `tool.load`. Current discovery takes precedence over a historical failure. A zero-match query is not proof that desktop operation is unavailable.
2. Use `computer.windows` for live windows, or `computer.apps` for known installed executables. These tools do not provide an exhaustive disk or process inventory. Use only a returned executable or an absolute `.exe` path supplied by the user. Never guess a window or process ID.
3. Retain the returned window ID and process ID together. A launch can reuse an existing process; use `computer.windows` when the actual target window is not yet identified. Check responsiveness and dimensions before dependent interaction, not as extra completion criteria.
4. Use `computer.read` when the requested operation needs accessible controls or existing receipts leave the target state unclear. It provides bounded UI Automation text, not the browser DOM or a complete inventory of background tabs.

## Open and arrange

- Use `computer.launch` with the known application path and ordinary application arguments. Keep its default background behavior unless the user explicitly requests foreground activation. A call reason cannot grant that permission. Do not use shell `start` or detached terminal scripts as another launch channel.
- Background launch aims to place the application behind the foreground window without minimizing it. Report only observed placement facts. A successful native launch receipt suffices for an ordinary open request; identify its window only when a requested subsequent interaction needs it. Deliver the launch result without adding UI or screenshot checks.
- The gateway declares Chrome/Edge's anti-occlusion rendering option before approval. Do not add a private login profile, disable browser protections or alter the user's browser connection to work around a failure.
- Use `computer.window` for resize, maximize, minimize or restore. Resize dimensions are physical client pixels; confirm the actual returned size and state. These operations do not request activation, but the current foreground policy can still block a state change. Respect that decision and use an allowed background operation or report the limitation. Keep a target restored when screenshots or accessible content are needed; a minimized window may not provide usable content.

## Capture and interact

- Use `computer.screenshot` only for a concrete visual question, requested visual quality or ambiguity that receipts and UI text cannot resolve; `question` can name the issue. It captures the identified client area as a PNG, optionally cropped. Reuse unchanged images. If only metadata arrived, use `image.read` for existing local pixels, not `artifact.open`, recapture or a request to reupload. Claim visual inspection only when a capable current model received pixels; report unsupported vision honestly.
- Native screenshots remain temporary and in the background. Permanent `retention` needs a nonempty `retentionReason` for user-requested preservation or key evidence; `image.retain(resultId,reason)` can preserve an existing result later. Retaining does not display an image or select a workspace tab. Display only requested screenshots or selected final deliverables. For a useful summary or key result, use `artifact.open(resultId)` to display the saved image without recapturing; avoid repeated workspace switches during execution.
- Prefer browser DOM operations for web controls. For a native control without a direct automation tool, read its current UIA bounds before choosing desktop input. Physical `computer.move`, `computer.click`, `computer.scroll`, `computer.drag`, `computer.type` and `computer.key` require the identified window in the foreground.
- Activate with `computer.activate` only within the user's allowed interaction, then check its receipt. If background-only operation is required and DOM or another suitable automation channel is unavailable, explain the limitation instead of taking focus.
- Coordinates are physical pixels relative to that window's client origin. Re-read after every layout change. Do not reuse browser CSS-pixel coordinates or a stale screenshot's coordinates.
- Authorized password form input is allowed; do not read back passwords or extract saved credentials. Connection trust, login and MFA may still need the user's action. Sending, publishing, deleting or changing accounts needs the corresponding user authorization.

Stop when trusted receipts and necessary observations establish the user's requested result; do not add acceptance criteria. A user's stop or deliver-existing instruction takes priority over further checks. An interrupted or `unknown` receipt requires checking the same operation and target before retrying. A screenshot alone cannot establish whether that dispatch succeeded. Preserve successful effects and report unresolved outcomes accurately.
