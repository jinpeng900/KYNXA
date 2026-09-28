# KYNXA Desktop Preview

This is a Linux-runnable visual and interaction preview of the Windows WinUI 3 shell in `../desktop`.
It is not a second production frontend and does not emulate Windows-only APIs.

## Run on Ubuntu

Node.js 22.19+ and a built DeepSeek Harness checkout are required for real model calls. Start the local model gateway first in a separate terminal:

```bash
cd apps/model-gateway
KYNXA_DSH_ROOT=/home/zhiyinpeng/deepseek-harness/deepseek-harness npm start
```

The gateway binds to `127.0.0.1:5218`. It stores connections in `~/.kynxa/harness` by default. The preview server proxies `/api/*` to this gateway so the API Key never needs to be kept in browser storage.

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
- Serves the approved logo and SVG icons directly from `../desktop/Resources/UI`; it does not duplicate them.
- Does not implement WinUI, Mica, AppWindow, WinRT, WebView2 or system tray behavior.

The WinUI project remains the source of truth. When its layout changes, update this preview deliberately.
