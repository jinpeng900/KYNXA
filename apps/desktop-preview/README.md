# KYNXA Desktop Preview

This is a Linux-runnable visual and interaction preview of the Windows WinUI 3 shell in `../desktop`.
It is not a second production frontend and does not emulate Windows-only APIs.

## Run on Ubuntu

Node.js 22.19+ is required for real model calls. KYNXA connects directly to the configured model API without an external SDK or source checkout. Start the local model gateway first in a separate terminal:

```bash
cd apps/model-gateway
npm start
```

The gateway binds to `127.0.0.1:5218`. It uses the shared data-root pointer in `~/.kynxa/storage.json` (`Models/` under that root), or the legacy `~/.kynxa/models` when unconfigured. The preview server proxies `/api/*` to this gateway so the API Key never needs to be kept in browser storage.

```bash
cd apps/desktop-preview
npm run dev:open
```

If the browser cannot be opened automatically:

```bash
npm run dev
```

Then visit `http://127.0.0.1:4173`.

Use a different port with:

```bash
npm run dev -- --port=5173
```

## Scope

- Mirrors the current shell dimensions, colors, sidebar, initial state and composer.
- Provides hover/pressed feedback, mode switching, menus, composer expansion, model configuration and real API-backed sending.
- Shows loading, unavailable, unconfigured and selected-model states. The refresh action checks the local gateway's model-list API; it does not certify that a model can generate a reply.
- Keeps separate work/chat conversations and drafts in page memory. Refreshing the page clears this preview history; it does not remove gateway data or desktop records. Sample projects are explicitly marked as demos and do not read local files.
- Provides copy, retry and stop controls. Stopping aborts the browser request and the gateway propagates cancellation to the upstream HTTP request. Whether generation stops immediately depends on the model service. Requesting again can start another model call.
- Uses a compact start guide, collapsible sidebar, optional work details, bounded keyboard-accessible menus and a single-column model editor on narrow windows. Work details show current page information and pending integration states; they do not invent task execution or files.
- Unimplemented navigation opens a planning explanation. The composer shows “文本对话” and explains the current capabilities; it does not execute tools or grant filesystem access.
- Keeps the conversation and composer in separate layout rows, including when the input area is expanded. Long replies scroll above the complete input and project controls.
- Searches the current mode's project/conversation titles and actual message text. Work and chat keep separate search queries. Demo project matches expand their conversation entries; message matches show a short text excerpt. Clearing a search restores the list, and an empty result explains how to try another query.
- Remembers each page session's reading position. A received reply preserves the viewport when reading older messages; the “回到最新” button resumes following the bottom. Sending a new question explicitly returns to the latest messages. Copy success appears briefly on the clicked button instead of a toast.
- Supports `Ctrl+N` to create a conversation in the current mode, `Ctrl+L` to focus the composer, and `Ctrl+F` to reveal and focus sidebar search. `Escape` closes an open menu first, then the narrow sidebar. These shell shortcuts do not run inside model or information dialogs.
- Serves the approved logo and SVG icons directly from `../desktop/Resources/UI`; it does not duplicate them.
- Does not implement WinUI, Mica, AppWindow, WinRT, WebView2 or system tray behavior.

The WinUI project remains the source of truth. When its layout changes, update this preview deliberately.

## Local regression checks

Run `node --test tests/*.test.mjs` from this directory. The model editor checks use a local fixture with no network calls or saved model credentials. They cover delayed provider loading, new connection ID collisions and editing existing connections.
