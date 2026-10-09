# Bundled runtime checks

Run on Windows from the repository root:

```powershell
powershell -File tests/node-runtime-smoke/check-cache.ps1 -VerifiedCacheDirectory 'apps/desktop/obj/node-runtime/win-x64'
powershell -File tests/node-runtime-smoke/check-pri.ps1
powershell -File tests/node-runtime-smoke/run.ps1 -BundleRoot '<fresh desktop build or publish directory>'
```

`check-cache.ps1` verifies the pinned runtime manifest and cache rejection/repair boundaries. `check-pri.ps1` verifies that the build filters only the separate `runtime/` and `ToolHost/` payloads out of WinUI resource scanning while preserving ordinary UI assets and the package exclusion list.

`run.ps1` copies the actual built `runtime`, `model-gateway`, and `ToolHost` directories to a temporary path containing spaces. It clears PATH, points DOTNET_ROOT at a nonexistent directory, and runs the copied Node/npm/npx, native capabilities, and real gateway with isolated temporary data and extensions. It compares the complete packaged official manifest to the repository source by SHA256, matches all 44 actual tool descriptor identities, and checks all 13 resource hashes across the seven official skills. Bilingual HTML conversion and the bundled parser license remain covered; the page fixture uses injected synthetic HTML and no network.

The copied Node also loads the bundled sqlite-vec DLL and USearch addon, then verifies FTS5, exact cosine vector search and ANN rankings using synthetic texts and vectors entirely in memory. The source embedding/reranker profile modules must match the copied profiles before all 11 E5 and eight reranker assets are checked for their pinned sizes and actual SHA256 hashes, including weights, tokenizers, model cards and licenses. This verifies asset integrity without loading models or running inference. The Rust resource-service executable, dependency notices and original licenses must be present; its `health`/`close` JSONL protocol must start and terminate successfully within the same cleared environment.

Fresh official MCP presets are enabled by default, but reading configuration and the tool catalog does not connect or start them; the fixture does not request MCP connections or model execution. Missing start commands, referenced credentials or required configuration paths are reported as not ready rather than usable. No user credentials, user chat or existing index is used; no external API is contacted. The owned temporary copied package, gateway logs and `portable-results.json` verification evidence remain at the printed path for diagnosis.

Use the fresh output root, not a stale AppX deployment directory. This check validates the selected platform's copied payload and does not install an MSIX or validate another architecture's device runtime.

Run this script again against the final publish output after publishing to recheck the shipped payload. `check-cache.ps1` and `check-pri.ps1` cover separate cache and WinUI resource-scanning boundaries; they need no change or repeated run solely because the portable retrieval checks were extended.
