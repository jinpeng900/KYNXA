# Native retrieval settings validation / 原生检索设置验证

```powershell
dotnet build tests/retrieval-ui-smoke/RetrievalUiSmoke.csproj -p:Platform=x64
```

Run the generated `RetrievalUiSmoke.exe`. The actual WinUI settings window is shown without activation, backed by an injected fake retrieval API. Checks cover automatic saves, revision conflicts, global inheritance/project overrides, mounted-folder opt-in, source ownership, background jobs, resizing, immediate language changes and closing during pending writes. / 启动生成的 `RetrievalUiSmoke.exe`。真实 WinUI 设置窗口以不抢焦点的方式显示，使用注入的虚构检索 API；检查自动保存、版本冲突、全局继承与工作覆盖、挂载文件夹显式启用、资料归属、后台任务、缩放、即时语言切换及待保存时关闭保护。

The latest result location is written to `%TEMP%/kynxa-retrieval-ui-smoke-latest.txt`. Native screenshots and JSON results stay in the owned temporary directory. These are automation-peer/control checks, not a claim of physical mouse/keyboard or native picker testing. / 最新结果位置写入 `%TEMP%/kynxa-retrieval-ui-smoke-latest.txt`，原生截图与 JSON 结果保留在独立临时目录；这些是自动化接口与真实控件检查，不宣称已验收物理鼠标、键盘或系统文件选择器。

## Additive indexing states / 新增索引状态

`App.JobStateChecks.cs` checks the existing status/refresh/cancel and real DispatcherTimer paths with synthetic receipts. It covers paused checkpoints, gateway-observed recovery to running, terminal partial coverage (lexical/semantic/failure/skipped/partial counts), omitted optional coverage, immediate Chinese/English translation with an older stored status snapshot, stopped terminal polling, owned paused cancellation and an empty refreshed job list. No UI action starts automatic recovery or creates a replacement task. / `App.JobStateChecks.cs` 使用合成回执检查现有状态读取、刷新、取消及真实 DispatcherTimer 轮询：暂停检查点、观察网关恢复运行、部分完成的关键词/语义/失败/跳过/部分覆盖数量、可选覆盖信息缺失、旧状态快照下即时中英文切换、终态停止轮询、按原任务 ID 取消暂停任务及刷新空任务列表。界面不自动恢复或创建替代任务。

The `partial-job-en.png` screenshot and named assertions are written beside `result.txt` and `result.json`. The fixture must be built and run to claim these checks passed; adding the assertions alone is not validation. / `partial-job-en.png` 截图及具名断言与 `result.txt`、`result.json` 保存在同一临时目录；必须实际构建运行后才能宣称通过，新增断言本身不代表验收完成。
