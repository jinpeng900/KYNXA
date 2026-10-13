# Retrieval client validation / 检索客户端验证

The console suite links the production C# transport and shared memory-domain contracts. / 控制台套件链接正式 C# 传输客户端与 Memory 领域共享合同。

```powershell
dotnet run --project tests/retrieval-client-smoke/RetrievalClientSmoke.csproj
dotnet run --project tests/retrieval-client-smoke/RetrievalClientSmoke.csproj -- --live --native-bundle '<built application directory>'
```

The default mode uses an HTTP recording handler and is reported separately from live integration. Live mode requires a declared bundle directory containing `runtime/agent/kynxa-gateway.exe`, the native resource service, SQLite/ORT libraries and ToolHost. `KYNXA_NATIVE_BUNDLE` may replace `--native-bundle`. No Node/.mjs process, model download or cloud/local model request is used. / 默认模式使用 HTTP 记录处理器，与 live 集成成绩分别报告；live 需要显式指定包含原生网关、资源服务、SQLite/ORT 库和 ToolHost 的包目录，可用 `KYNXA_NATIVE_BUNDLE` 代替参数，不运行 Node/.mjs、不下载模型，也不请求云端或本地生成模型。

Live mode creates independent temporary Data/Models/extensions, synthetic projects/chats and three text sources. It starts the actual Rust executable on an ephemeral loopback port with semantic inference disabled, then checks production C# DTO decoding, two consecutive global/project settings CAS writes, actual SQLite publication, source/search isolation, memory lifecycle and durable-job cancellation. Asset availability does not imply model loading. These checks do not measure semantic retrieval quality or Agent task completion. / live 使用独立临时数据、模型及扩展目录、虚构工作/聊天和三份资料，在随机回环端口启动真实 Rust 程序且关闭语义推理，检查正式 C# DTO、连续两次配置 CAS、实际 SQLite 发布、范围隔离、记忆生命周期及持久任务取消；资产可用不能当成模型已加载，成绩不代表语义质量或 Agent 任务完成率。

Cleanup validates the exact owned temporary directory and terminates only its owned process tree. The former Node fixture is archived under `artifacts/development/node-removal-20261012/legacy-tests/retrieval-client-smoke/`. / 清理核验精确自有临时路径，只终止自有进程树；旧 Node 夹具保存在上述隔离历史目录。
