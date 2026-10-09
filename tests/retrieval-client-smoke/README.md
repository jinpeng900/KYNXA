# Retrieval client validation / 检索客户端验证

The console suite links the production C# transport and shared memory-domain contracts. / 控制台套件链接正式 C# 传输客户端与 Memory 领域共享合同。

```powershell
dotnet run --project tests/retrieval-client-smoke/RetrievalClientSmoke.csproj
dotnet run --project tests/retrieval-client-smoke/RetrievalClientSmoke.csproj -- --live
```

The default mode uses an HTTP recording handler to check revisions, scope identities, inheritance clearing, optional fields and cancellation. `--live` starts the actual gateway on an ephemeral loopback port, with only synthetic project/chat records, text sources and an intentionally unavailable embedding model. It exercises JSON compatibility, SQLite indexing jobs, source revocation and unchanged conversation state. No cloud model, credentials or external MCP process is used. / 默认模式用 HTTP 记录处理器检查版本、范围身份、继承重置、可选字段与取消。`--live` 在随机回环端口启动真实网关，仅使用虚构工作、聊天、文本资料与故意缺失的嵌入模型，验证 JSON 兼容、SQLite 索引任务、资料撤销及聊天状态保持；不使用云模型、凭据或外部 MCP 进程。

Cleanup targets only the fixture's verified temporary directory and its owned process. The suite needs the repository Node dependencies restored. / 清理仅针对已校验的夹具临时目录及自有进程；套件需预先还原仓库 Node 依赖。

The live fixture expects `partial` after each import because embedding assets are intentionally missing. It verifies exact eligible-source counts (one global source, then two including inherited global/project sources), complete lexical publication, zero semantic coverage, incomplete coverage and `EMBEDDING_ASSET_MISSING` diagnostics. The waiter returns the terminal receipt for these assertions; `failed` and unexpected `cancelled` remain failures, while `paused` remains pending. Manual rebuilding after both sources are removed must finish or acknowledge the explicit cancellation. / 真实联调夹具故意缺少嵌入资产，所以逐次导入预期为 `partial`：精确验证全局一次 1 个来源、工作含继承全局共 2 个来源、全部词法发布、语义覆盖为零、覆盖不完整以及 `EMBEDDING_ASSET_MISSING` 诊断。等待器返回终态回执供这些断言核对；`failed` 和未经允许的 `cancelled` 仍失败，`paused` 继续等待。移除两个来源后的手动重建必须完成或确认显式取消，不能把任意部分成功视为完整通过。
