# Runtime verification

Run the small contract suite on Windows with .NET 10 and Node.js:

```powershell
dotnet run --project tools/verification/runtime/RuntimeSmoke.csproj --configuration Release
node tools/verification/transcript-smoke.mjs
```

It links production stream parsing, reply recovery, message timing, task observation and Markdown formatting. Only HTTP responses and desktop startup are substituted. It needs no API key, does not send model requests and does not touch user data. These checks do not replace interactive WinUI testing.

The Windows workflow also loads the actual transcript HTML and JavaScript in an isolated headless Chromium profile. It checks selection, terminal status, long-body patches and navigation; it does not instantiate WinUI controls. To run it locally:

```powershell
node tools/verification/transcript-browser-smoke.mjs "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
```

A manual workflow run also checks native storage. Locally, with the Rust MSVC environment prepared:

```powershell
cargo test --manifest-path apps/agent-core/Cargo.toml --locked --test runtime_recovery
cargo test --manifest-path apps/agent-core/Cargo.toml --locked --test local_download download
cargo test --manifest-path apps/agent-core/Cargo.toml --locked --test model_slots
```

Native checks use temporary directories to verify original-reply lookup, source changes, conversation isolation and pause/restart/resume with saved context and successful receipts. Each fixture removes only its own checked temporary directory. Build outputs remain in ignored `bin`, `obj` and `target` directories.

Model-slot checks cover owned single-slot admission, loopback API relays, external slot reports, cancellation, foreground/background fairness and a real pair of concurrent HTTP requests against a temporary loopback server. They do not start an inference model or consume provider credits.

Download checks compile the production modules directly. Local HTTP fixtures cover mirror preference, slow-source fallback, concurrent ranges, partial-range resumption, 429 backoff, ignored/mismatched Range responses, ownership markers and failed integrity checks. Injected-clock checks cover growth beyond sixteen connections, throughput rollback, resource pressure and chat priority. Shared-admission fixtures run with `-- --test-threads=1` to avoid competing with one another for the process resource budget. Fixture-only timing and segment sizes are shortened; no real model or proxy setting is changed.

## Scope / 范围

验证终态到达后不重发请求、暂停后不因查看历史自动续跑、成功回执与累计时间保留。已知回复通过消息 ID 查询；查询仍核对会话归属、来源版本并过滤私有转录。工具或计时变化使用正文复用补丁；切换聊天、正文变化或流结束照常发送完整内容。

本套验证使用真实生产模块和模拟 HTTP 回执，不代表真实供应商、完整桌面交互或断电恢复实测。原任务的排队、引导、权限、上下文及模型缓存规则保持独立。
