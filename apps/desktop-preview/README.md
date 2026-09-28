# KYNXA Desktop Preview

This is a Linux-runnable visual and interaction preview of the Windows WinUI 3 shell in `../desktop`.
It is not a second production frontend and does not emulate Windows-only APIs.

## Run on Ubuntu

Node.js 18 or newer is the only requirement. No package installation is needed.

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
- Provides hover/pressed feedback, mode switching, menus, composer expansion and mock sending.
- Serves the approved logo and SVG icons directly from `../desktop/Resources/UI`; it does not duplicate them.
- Does not implement WinUI, Mica, AppWindow, WinRT, WebView2, system tray or backend behavior.

The WinUI project remains the source of truth. When its layout changes, update this preview deliberately.
