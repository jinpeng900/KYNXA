# Bundled runtime checks

Run on Windows from the repository root:

```powershell
powershell -File tests/node-runtime-smoke/check-cache.ps1 -VerifiedCacheDirectory 'apps/desktop/obj/node-runtime/win-x64'
powershell -File tests/node-runtime-smoke/check-pri.ps1
powershell -File tests/node-runtime-smoke/run.ps1 -BundleRoot '<fresh desktop build or publish directory>'
```

`check-cache.ps1` verifies the pinned runtime manifest and cache rejection/repair boundaries. `check-pri.ps1` verifies that the build filters only the separate `runtime/` and `ToolHost/` payloads out of WinUI resource scanning while preserving ordinary UI assets and the package exclusion list.

`run.ps1` copies the actual built `runtime`, `model-gateway`, and `ToolHost` directories to a temporary path containing spaces. It clears PATH, points DOTNET_ROOT at a nonexistent directory, and runs the copied Node/npm/npx, native capabilities, and real gateway with isolated temporary data and extensions. It also checks all seven official skill resource hashes, 35 core tools, bilingual HTML conversion and the bundled parser license. The page-reading fixture uses injected synthetic HTML and no network. All default MCP presets remain disabled; no model, user credentials, or user chat is used.

Use the fresh output root, not a stale AppX deployment directory. This check validates the selected platform's copied payload and does not install an MSIX or validate another architecture's device runtime.
