# Screenshot panel UI smoke

Run `powershell -File tests/screenshot-panel-ui-smoke/run.ps1` from the repository root on Windows. Run this native window test separately from other foreground UI checks.

The fixture uses the production screenshot panel, image decoder, source projection, UI resources, and language service. Its injected API serves synthetic PNG archives in an isolated temporary data directory and deliberately returns responses after cancellation. It never reads user chats, invokes a model, connects MCP, or captures the user's desktop.

Checks cover receipt identity and size limits; lazy loading; synchronous sidebar opening without duplicate reads; conversation changes; cancelled and late responses; archive-only retry; bounded thumbnail caching; manual navigation; hot language changes; narrow layout; and subscription disposal. Shared decoding checks also cover JPEG/GIF compatibility and image bounds. The existing `agent-ui-smoke --computer-tools-only` verifies the full screenshot viewer and computer approvals.

`App.MountedWorkspaceChecks.cs` also hosts the production mounted-folder header. It verifies the folder icon and basename, full-path tooltip, hidden state without a folder, actual native open/change/unlink events, live localization, narrow layout, unchanged metadata refreshes, invalidated menus after project/path changes, and disposal. These callbacks are observed by the fixture; it does not open Explorer, invoke the folder picker, or change the formal catalog.

The combined check uses the same 32-pixel header row and margins as `ShellPage.xaml`. It displays an archived PNG under the mounted folder, verifies the visible geometry and uncropped width, and writes `mounted-screenshot-combined.png`.

`App.ScreenshotViewerChecks.cs` tests responsive large-image layout and the production independent full-screen viewer. Synthetic 3000×1500, 3840×2160 and long-page archives verify aspect ratio, sidebar centering, bounded decode upgrades without repeat API reads, original pixels, DPI-aware 100% size, Fit, hot language changes, and cancellation both before and after image loading. Physical wheel, drag and Escape are exercised only when Windows grants the fixture foreground; otherwise the result explicitly records `SKIP`, sends no global inputs, and still checks native UI Automation panning and the close button. The previous cursor position is restored after permitted physical inputs. These checks must not run alongside another foreground UI or desktop input fixture.

`App.ShellScreenshotRoutingChecks.cs` compiles the actual `ShellPage.AgentResults.cs` with a minimal event-owning fixture host. It exercises custom MCP screenshot routing, current message/chat identity, duplicate-window rejection and chat-change cancellation. The host substitutes the transcript's conversation-change event and API, not the production routing implementation.

`App.BrowserScreenshotChecks.cs` verifies custom-named Playwright and Chrome MCP screenshot receipts, PNG/JPEG thumbnails, embedded remote image archives, and the actual concise screenshot dialog. Browser file links are preserved alongside archived typed pixels; neither the gallery nor the dialog reads returned filenames or downloads URLs.

An optional real-browser check is `node tests/screenshot-panel-ui-smoke/browser-artifacts-live-smoke.mjs`. Explicitly supply `KYNXA_LIVE_BROWSER_PATH` and an already installed `KYNXA_LIVE_PLAYWRIGHT_ENTRY` and/or `KYNXA_LIVE_CHROME_DEVTOOLS_ENTRY`. It uses the production MCP client, fresh isolated headless profiles, a loopback HTML fixture, and temporary files. It verifies filename-only PNG/JPEG output is archived as typed media while model previews contain no image pixels. It does not install dependencies, reuse a signed-in profile, or call a model.

Pass `--remote-cdp` to verify both MCPs attach to a separately owned headless Chrome debugging endpoint. The fixture checks navigation, accessibility DOM reading, and standard PNG/JPEG image archives; its own Chrome profile and process are removed from active use afterward. This exercises the remote CDP protocol through a loopback fixture, not an unconfigured cloud provider.

The result path is written to `%TEMP%/kynxa-screenshot-panel-latest.txt`. The fixture's unique temporary directory contains `result.txt`, screenshot panel previews, `mounted-header-zh.png`, and `mounted-header-narrow-en.png`; inspect these actual native window captures when changing layout.
