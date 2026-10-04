# Native desktop verification

These checks use only windows, child processes and browser profiles created by the fixture. They must run serially with other foreground UI tests. They do not inspect a user's windows, profile, credentials, chat data or model API. Screenshots/read results are written into unique `%TEMP%/kynxa-native-*` artifact folders; they are never committed.

Build the self-contained helper first:

```powershell
dotnet build apps/tool-host/KYNXA.ToolHost.csproj -c Debug -r win-x64 --self-contained true
```

Pure partial-input verification performs no desktop interaction, using the production prefix tracker with an injected sender:

```powershell
dotnet run --project tests/native-desktop-smoke/InputSequenceSmoke.csproj -c Debug
```

It covers partial mouse clicks, all prefixes of a key chord, Unicode release, successful drag cancellation, release retries, zero deliveries, middle/right buttons and explicit failure when cleanup remains incomplete. The current run passed 24 checks.

The WPF fixture provides real text/password/URL controls, button, drag region and scroller:

```powershell
dotnet run --project tests/native-desktop-smoke/NativeDesktopSmoke.csproj -c Debug
dotnet run --project tests/native-desktop-smoke/NativeDesktopSmoke.csproj -c Debug -- --input-extensions-only
```

The base run initially passed 26 actual checks: window/PID scope, visible UIA text and URL, password exclusion, PNG bytes/dimensions, invalid target/key/coordinate and host-shell rejection, foreground refusal, actual bilingual typing, mouse move/click, scrolling/dragging and launching an owned GUI child. The extension run checks registry application discovery, overlong input refusal without mutation, bounded read validation, minimized-window activation and actual middle click. Both shut down only their own windows/processes.

The real-browser check uses installed Edge/Chrome executables, a fresh user-data directory and a locally served synthetic page:

```powershell
node tests/native-desktop-smoke/browser-smoke.mjs
```

It invokes the production DesktopRunner and real ToolHost GUI launch/windows/read/screenshot channels. It verifies best-effort background startup receipts, bilingual visible document text, the exact loopback address, password locator metadata without Name/Value/text and completed host-desktop receipts. The owned fresh-profile page launch explicitly enables `--disable-backgrounding-occluded-windows` to keep its completely covered renderer/accessibility provider running; without that option this update reproduced blank page screenshots and missing document UIA. Chromium initialization is polled briefly rather than assumed synchronous. The current actual Edge + Chrome run passed 42 checks, and both rendered page PNGs were inspected. The count varies with initialization retries; all functional assertions remain. Cleanup terminates only the browser PID returned for the new temporary profile and its own descendants.

For a helper at another build path set `KYNXA_DESKTOP_SMOKE_TOOL_HOST` to the absolute executable path. Native tests require an unlocked interactive Windows session. Missing installed browsers is a failed prerequisite, not a claimed browser pass. No screenshot or read of an existing user's browser is permitted in these fixtures.

The capability extension fixture verifies bounded element/region reads, password locator metadata without Name/Value/text, original-pixel screenshot crop, non-activating window resize/maximize/minimize/restore, a best-effort background GUI launch, an unresponsive UI thread and a synthetic stalled UIA provider:

```powershell
dotnet run --project tests/native-desktop-smoke/NativeDesktopSmoke.csproj -- --capability-readonly-only
dotnet run --project tests/native-desktop-smoke/NativeDesktopSmoke.csproj -- --capability-extensions-only
```

The read-only run passed 35 real Windows checks in this update. The full variant additionally requires the owned fixture to obtain foreground through ordinary `Window.Activate`; it verifies actual password input and zoom key events. Windows foreground refusal is a failed prerequisite, not a claimed input pass or a reason to bypass the protection.

The background-placement fixture creates an intentionally delayed activating GUI child and a real visible host console. It verifies the default background launch, confirmed own-window Z-order behind the fixture, foreground preservation through resize/maximize/minimize/restore, original target screenshot pixels while obscured, console output/completion, and target-only console screenshot without activation:

```powershell
dotnet run --project tests/native-desktop-smoke/NativeDesktopSmoke.csproj -c Debug -- --background-placement-only
```

This change passed 30 actual Windows checks. The resulting `background-target.png` (LightYellow child distinct from the white foreground fixture) and `background-console.png` were visually checked. The bounded launch observation is 1,500 ms; application focus overrides after that interval remain a best-effort limitation, not permanent focus suppression.
