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

## Identify and observe

1. Discover current tools with `tool.search` and load deferred exact names with `tool.load`. Current discovery takes precedence over a historical failure. A zero-match query is not proof that desktop operation is unavailable.
2. Use `computer.windows` for live windows, or `computer.apps` for known installed executables. These tools do not provide an exhaustive disk or process inventory. Use only a returned executable or an absolute `.exe` path supplied by the user. Never guess a window or process ID.
3. Retain the returned window ID and process ID together. Inspect `isResponding` and current client dimensions before continuing. A launched application can reuse an existing process, so list windows again instead of treating the launch PID as the target window's identity.
4. Use `computer.read` for bounded UI Automation text and accessible controls in the identified window. Its data is not the browser DOM or a complete inventory of background tabs. Refresh after navigation, a dialog, scrolling or a size change.

## Open and arrange

- Use `computer.launch` with the known application path and ordinary application arguments. Keep its default background behavior unless the user explicitly requests foreground activation. A call reason cannot grant that permission. Do not use shell `start` or detached terminal scripts as another launch channel.
- Background launch aims to place the application behind the foreground window without minimizing it. Inspect the returned placement and foreground observations; never claim it succeeded merely because a process started. Applications can activate themselves later, and an existing browser process can ignore new launch arguments.
- The gateway declares Chrome/Edge's anti-occlusion rendering option before approval. Do not add a private login profile, disable browser protections or alter the user's browser connection to work around a failure.
- Use `computer.window` for resize, maximize, minimize or restore. Resize dimensions are physical client pixels; confirm the actual returned size and state. These operations do not request activation, but the current foreground policy can still block a state change. Respect that decision and use an allowed background operation or report the limitation. Keep a target restored when screenshots or accessible content are needed; a minimized window may not provide usable content.

## Capture and interact

- `computer.screenshot` captures the identified client area as an original-resolution PNG, optionally with a physical-pixel crop. The application archives it for the sidebar. This text-only model receives metadata, not image pixels; use observed UI text unless image understanding is actually available. A screenshot acknowledgement is not visual evidence of its contents.
- Prefer browser DOM operations for web controls. For a native control without a direct automation tool, read its current UIA bounds before choosing desktop input. Physical `computer.move`, `computer.click`, `computer.scroll`, `computer.drag`, `computer.type` and `computer.key` require the identified window in the foreground.
- Activate with `computer.activate` only within the user's allowed interaction, then check its receipt. If background-only operation is required and DOM or another suitable automation channel is unavailable, explain the limitation instead of taking focus.
- Coordinates are physical pixels relative to that window's client origin. Re-read after every layout change. Do not reuse browser CSS-pixel coordinates or a stale screenshot's coordinates.
- Authorized password form input is allowed; do not read back passwords or extract saved credentials. Connection trust, login and MFA may still need the user's action. Sending, publishing, deleting or changing accounts needs the corresponding user authorization.

Verify the changed UI state after each effect. An interrupted or `unknown` receipt does not prove that nothing happened: observe the same target before retrying. Report the actual outcome and any blocked operation; never claim an application was closed, a task completed or a screenshot visually inspected without its evidence.
